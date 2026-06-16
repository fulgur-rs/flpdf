#[path = "writer/object_streams.rs"]
pub(crate) mod object_streams;
pub use object_streams::ObjectStreamMode;

use crate::parser::Parser;
use crate::signatures::{signature_rewrite_impact, SignatureWriteMode};
use crate::{filters, Dictionary, Object, ObjectRef, Pdf, Result, XrefForm, XrefOffset};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};

/// Controls whether the full-rewrite path applies FlateDecode compression to
/// output streams.
///
/// # Byte-vs-observable policy
///
/// flpdf uses zlib (via the `flate2` crate) with `Compression::default()`,
/// which selects a different compression level and block layout than qpdf's
/// internal zlib build.  As a result, **flpdf's FlateDecode output is
/// observably equivalent to qpdf's (same decoded bytes) but will not be
/// byte-identical**.  The acceptance criterion for this toggle is round-trip
/// correctness (decoded bytes match), not byte-identical agreement with qpdf.
///
/// This tradeoff is intentional and documented here to avoid spending time
/// chasing byte-level zlib parity, which would require re-implementing qpdf's
/// exact compression parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressStreams {
    /// Apply FlateDecode to every output stream that does not already carry a
    /// filter chain flpdf cannot re-encode (e.g. DCTDecode, JPXDecode).
    ///
    /// For the full-rewrite path this means: decode the source stream through
    /// its declared filter pipeline and re-emit the result with a single
    /// `/FlateDecode` filter.  Streams whose decode or re-encode fails are
    /// emitted verbatim (the fallback preserves readability).
    ///
    /// This is the default — matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    #[default]
    Yes,
    /// Emit every output stream without any FlateDecode compression.
    ///
    /// For the full-rewrite path: decode the source stream and write the raw
    /// bytes without any `/Filter`.  Streams whose decode fails (e.g. because
    /// the declared filter is `DCTDecode` / `JPXDecode` and the image data is
    /// opaque to flpdf) are passed through verbatim — their original `/Filter`
    /// chain is preserved so the output remains readable.
    No,
}

/// Controls how the full-rewrite path handles stream data.
///
/// This is the higher-level policy that mirrors qpdf's `--stream-data` option.
/// When set on [`WriteOptions`], it **overrides** [`WriteOptions::compress_streams`]
/// for regular indirect streams (non-xref, non-ObjStm container bodies).
///
/// # Semantics
///
/// | Variant      | Equivalent `CompressStreams` | Behaviour |
/// |-------------|-------------------------------|-----------|
/// | `Preserve`  | bypass (no decode/re-encode)  | Pass dict + raw data verbatim; `apply_stream_compress_policy` is not called |
/// | `Uncompress`| `CompressStreams::No`         | Decode through all declared filters, emit raw bytes without any `/Filter` |
/// | `Compress`  | `CompressStreams::Yes`        | Decode, then re-encode with a single `/FlateDecode` filter |
///
/// # Interaction with `--compress-streams`
///
/// When `WriteOptions::stream_data` is `Some(mode)`, the mode takes precedence
/// over `WriteOptions::compress_streams` for per-object stream bodies.
/// Structural streams (xref streams, ObjStm containers) continue to use
/// `compress_streams` regardless of `stream_data`.
///
/// # Interaction with QDF mode
///
/// When `WriteOptions::qdf` is `true`, QDF wins: every applicable stream is
/// decoded to raw bytes (equivalent to `Uncompress`), overriding even
/// `stream_data = Some(Preserve)`.  This matches qpdf's behaviour where `--qdf`
/// takes precedence over `--stream-data=preserve`.
///
/// # Default
///
/// The default is `None` on [`WriteOptions`] — no `stream_data` is set — which
/// preserves full backward compatibility with the existing `compress_streams`
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDataMode {
    /// Pass streams through verbatim — no decode or re-encode.
    ///
    /// The stream dictionary and raw data bytes are emitted unchanged.  This
    /// bypasses [`apply_stream_compress_policy`] entirely, so a stream carrying
    /// `/Filter /FlateDecode` will still carry that filter in the output.
    Preserve,
    /// Decode and emit raw bytes without any `/Filter`.
    ///
    /// Equivalent to `CompressStreams::No`: the declared filter chain is decoded
    /// and the raw bytes are written without any `/Filter` or `/DecodeParms`.
    /// Streams that cannot be decoded (e.g. DCTDecode) are emitted verbatim.
    Uncompress,
    /// Decode and re-encode with a single `/FlateDecode` filter.
    ///
    /// Equivalent to `CompressStreams::Yes`: the declared filter chain is decoded
    /// and the result is re-encoded with FlateDecode.
    Compress,
}

/// Compute the effective stream policy for regular indirect streams.
///
/// Returns `Some(policy)` meaning "call `apply_stream_compress_policy` with
/// this policy", or `None` meaning "preserve mode: skip decode/re-encode and
/// emit the stream verbatim".
///
/// # Priority
///
/// 1. QDF mode (`options.qdf`) always returns `Some(CompressStreams::No)` —
///    QDF requires fully decoded streams regardless of `stream_data`.
/// 2. `options.stream_data = Some(mode)` overrides `options.compress_streams`.
/// 3. `options.stream_data = None` falls back to `options.compress_streams`.
pub(crate) fn effective_stream_policy(options: &WriteOptions) -> Option<CompressStreams> {
    if options.qdf {
        return Some(CompressStreams::No);
    }
    match options.stream_data {
        Some(StreamDataMode::Preserve) => None,
        Some(StreamDataMode::Uncompress) => Some(CompressStreams::No),
        Some(StreamDataMode::Compress) => Some(CompressStreams::Yes),
        None => Some(options.compress_streams),
    }
}

/// Controls whether a newline is inserted immediately before the `endstream`
/// keyword.
///
/// ISO 32000-1 §7.3.8.1 recommends an end-of-line marker before `endstream`.
/// In all variants the `/Length` dictionary entry reflects the raw payload
/// length only — never any inserted newline.
///
/// # Variants and qpdf equivalence
///
/// - [`Yes`](Self::Yes) (the **flpdf default**) — always write exactly one
///   `b'\n'`, satisfying the ISO 32000-1 §7.3.8.1 recommendation and easing
///   hand-editing. Equivalent to qpdf run **with** `--newline-before-endstream`.
/// - [`No`](Self::No) — write a single `b'\n'` only when the payload does not
///   already end with `\n`/`\r`; if it does, `endstream` is adjacent. This is a
///   flpdf-specific middle ground and matches neither of qpdf's two modes.
/// - [`Never`](Self::Never) — never insert a newline; exactly the raw payload
///   bytes sit between `stream` and `endstream`. This reproduces qpdf's
///   **default** output (qpdf only inserts a newline when run with
///   `--newline-before-endstream`), and is required for byte-identical
///   `qpdf`-equivalent output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum NewlineBeforeEndstream {
    /// Always write exactly one `b'\n'` before `endstream` (the flpdf default),
    /// regardless of whether the payload already ends with a newline.
    ///
    /// Satisfies ISO 32000-1 §7.3.8.1 and matches qpdf run with
    /// `--newline-before-endstream`.
    #[default]
    Yes,
    /// Write a single `b'\n'` before `endstream` only when the payload does not
    /// already end with `\n`/`\r`; otherwise `endstream` is adjacent.
    ///
    /// A flpdf-specific parseability-preserving middle ground (it matches
    /// neither of qpdf's two modes).
    No,
    /// Never insert a newline: the raw payload is written verbatim and
    /// `endstream` follows immediately, so exactly `/Length` bytes sit between
    /// `stream` and `endstream`.
    ///
    /// Reproduces qpdf's default output and is required for byte-identical
    /// qpdf-equivalent rewrites.
    Never,
}

/// Options controlling [`write_pdf_with_options`].
///
/// Constructed via `Default::default()` or struct literal. The struct is
/// `#[non_exhaustive]` so additional fields can be added without breaking
/// existing callers.
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct WriteOptions {
    /// Override the trailer `/ID`'s second element (the changing identifier)
    /// with qpdf's static-id constant — the first 32 hex digits of π. The
    /// first element (the permanent identifier) is preserved from the input
    /// trailer when present; if absent, both elements are set to the constant.
    /// Mirrors `qpdf --static-id` and is intended for byte-identical testing.
    pub static_id: bool,

    /// Derive the trailer `/ID[1]` (the changing identifier) from an MD5 digest
    /// of the rewritten output body — the bytes from the file header through the
    /// last body object, up to (but not including) the cross-reference table —
    /// so the identifier is stable across runs for identical input and flags.
    /// The permanent identifier `/ID[0]` is preserved from the input (ISO
    /// 32000-1 §14.4), falling back to the digest when the input has no usable
    /// `/ID`. Like `qpdf --deterministic-id`, this yields a content-derived,
    /// run-stable `/ID` and preserves the permanent identifier.
    ///
    /// Only the full-rewrite path honours this flag; requesting it on the
    /// incremental-update path is rejected. It is mutually exclusive with
    /// [`WriteOptions::static_id`] and is rejected for encrypted output (the
    /// `/ID` feeds the encryption key, so a content-derived `/ID` would be
    /// circular) — both matching qpdf.
    ///
    /// The digest is flpdf's own scheme (a single MD5 over the body); it is
    /// **not** byte-identical to the value qpdf writes, which seeds a second MD5
    /// with the body digest plus the `/Info` strings. The `/ID` is therefore
    /// self-stable and qpdf-equivalent in behaviour, but not in exact bytes.
    pub deterministic_id: bool,

    /// Force every AES CBC IV to `0x00 × 16` instead of a cryptographically
    /// random value.
    ///
    /// **TESTING ONLY — NOT for production.**  When `true`, both stream-level
    /// and string-level AES encryption (AES-128 CBC and future AES-256 CBC)
    /// use an all-zero IV, making the ciphertext deterministic and enabling
    /// byte-identical output tests.  Without this flag (the default `false`)
    /// every encryption call generates a fresh random IV via the OS CSPRNG.
    ///
    /// Mirrors `qpdf --static-aes-iv`.  Must never be set in production code;
    /// deterministic IVs make AES CBC completely insecure.
    pub static_aes_iv: bool,

    /// Enforce a minimum PDF version in the output header.
    ///
    /// The effective version is `max(source_version, min_version)`.  Only
    /// applies to the new-generation write paths (`write_qdf`, linearize);
    /// the incremental-update path (`write_pdf`) leaves the source header
    /// untouched.  Format: `"1.3"`, `"1.7"`, etc.
    ///
    /// Mirrors `qpdf --min-version`.
    pub min_version: Option<String>,

    /// Force the output PDF version header to exactly this value, ignoring the
    /// source version and the linearize floor.
    ///
    /// Mirrors `qpdf --force-version`.
    pub force_version: Option<String>,

    /// When `true`, suppress the `%% Original object ID: N M` comments that the
    /// QDF writer would otherwise emit before each object.
    ///
    /// Mirrors `qpdf --no-original-object-ids`. qpdf's own help: *"Omit
    /// comments in a QDF file indicating the object ID an object had in the
    /// original file."* Observed against qpdf 11.9.0, this flag affects **only**
    /// QDF output (`qpdf --qdf` vs `qpdf --qdf --no-original-object-ids`); JSON
    /// v1 and v2 output are byte-identical with or without it, so this field is
    /// intentionally **not** wired into any JSON path.
    ///
    /// `flpdf`'s full-rewrite path (`write_pdf_full_rewrite`, reached when
    /// `full_rewrite = true`) emits `%% Original object ID: N G` immediately
    /// before each indirect object's `N G obj` line when `qdf = true` and this
    /// flag is `false`.  Setting this flag to `true` suppresses those comments
    /// while leaving the `N G obj` lines intact — matching qpdf's
    /// `--no-original-object-ids` behaviour exactly.
    pub no_original_object_ids: bool,

    /// When `true`, decode every stream through its filter pipeline and re-emit
    /// the document end-to-end with a single `/FlateDecode` filter applied to
    /// each stream.  The output contains no `/Prev` chain, no `ObjStm`, and no
    /// xref-stream-only keys.
    ///
    /// When `false` (the default) the existing incremental-update write path is
    /// used, preserving the source bytes verbatim.
    pub full_rewrite: bool,

    /// Allow the full-rewrite path to proceed even when it invalidates existing
    /// signed byte ranges.
    ///
    /// The default `false` refuses signed PDFs with [`crate::Error::Signed`].
    /// Set this only when the caller is explicitly performing a destructive
    /// rewrite and accepts that existing signatures will no longer validate.
    pub allow_signed_full_rewrite: bool,

    /// Object stream emission policy for the output.
    ///
    /// Mirrors `qpdf --object-streams=preserve|disable|generate`. Defaults to
    /// [`ObjectStreamMode::Preserve`], matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    ///
    /// Only consulted by writer paths that emit ObjStms; the incremental copy
    /// path that simply appends to the source bytes ignores it.
    pub object_streams: ObjectStreamMode,

    /// Stream compression policy for the full-rewrite path.
    ///
    /// [`CompressStreams::Yes`] (the default) decodes each stream and
    /// re-encodes it with a single `/FlateDecode` filter, matching qpdf's
    /// default behaviour.  [`CompressStreams::No`] decodes each stream and
    /// emits the raw bytes without any filter; streams that cannot be decoded
    /// (e.g. `DCTDecode`/`JPXDecode` image data) are passed through verbatim.
    ///
    /// Only consulted by the full-rewrite path (`full_rewrite = true`);
    /// it governs regular indirect streams, ObjStm containers, and the
    /// xref stream alike. The incremental-update path and `write_qdf`
    /// are unaffected.
    pub compress_streams: CompressStreams,

    /// Whether to insert a newline immediately before each `endstream` keyword.
    ///
    /// ISO 32000-1 §7.3.8.1 requires an end-of-line marker before `endstream`.
    /// [`NewlineBeforeEndstream::Yes`] (the default) always writes exactly one
    /// `b'\n'` before `endstream`, matching qpdf's `--newline-before-endstream=y`
    /// behaviour.  [`NewlineBeforeEndstream::No`] omits the extra newline when
    /// the stream payload already ends with a newline character.
    ///
    /// The `/Length` value in the stream dictionary is **not** affected by this
    /// setting — it always reflects the raw payload byte count only.
    ///
    /// Applied to every stream in the full-rewrite output.  The incremental-update
    /// path emits streams verbatim from the input and is unaffected.
    pub newline_before_endstream: NewlineBeforeEndstream,

    /// Emit the document in QDF (Query Data Format) mode.
    ///
    /// When `true` (and `full_rewrite` is also `true`), every stream that uses a
    /// "safe text" filter chain — [`FlateDecode`], [`LZWDecode`], [`ASCIIHexDecode`],
    /// [`ASCII85Decode`], [`RunLengthDecode`] — is fully decoded and written as raw
    /// bytes.  The `/Filter` and `/DecodeParms` entries are removed from the stream
    /// dictionary and `/Length` is updated to the decoded byte count, making the
    /// stream data human-readable in a text editor.
    ///
    /// Image/binary codecs that flpdf cannot decompress — `DCTDecode`, `JBIG2Decode`,
    /// `JPXDecode`, `CCITTFaxDecode` — and any unknown filter are left **untouched**:
    /// the compressed bytes and the original `/Filter` chain are preserved verbatim.
    /// This matches qpdf's own QDF behaviour.
    ///
    /// When `true`, this setting takes precedence over [`compress_streams`] for the
    /// per-object stream emission: the stream is always emitted decompressed regardless
    /// of the `compress_streams` value.  The xref stream and ObjStm containers are
    /// governed solely by `compress_streams` and are not affected by this flag.
    ///
    /// This field is the library-level entry point for QDF mode.  Test via
    /// `WriteOptions { qdf: true, full_rewrite: true, .. }`.
    ///
    /// [`FlateDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`LZWDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`ASCIIHexDecode`]: https://pdf.pizza/spec/7.4.2
    /// [`ASCII85Decode`]: https://pdf.pizza/spec/7.4.3
    /// [`RunLengthDecode`]: https://pdf.pizza/spec/7.4.5
    /// [`compress_streams`]: WriteOptions::compress_streams
    pub qdf: bool,

    /// Higher-level stream data policy (qpdf `--stream-data={preserve,uncompress,compress}`).
    ///
    /// When set, this overrides [`compress_streams`] for regular indirect stream bodies.
    /// Structural streams (xref streams and ObjStm containers) are not affected and
    /// continue to use [`compress_streams`].
    ///
    /// | Value                          | Effect on regular streams            |
    /// |-------------------------------|--------------------------------------|
    /// | `None` (default)              | Fall back to `compress_streams`      |
    /// | `Some(StreamDataMode::Preserve)` | Emit dict + raw bytes verbatim    |
    /// | `Some(StreamDataMode::Uncompress)` | Decode, emit raw (no `/Filter`) |
    /// | `Some(StreamDataMode::Compress)`   | Decode, re-encode with FlateDecode |
    ///
    /// **Note:** when `qdf = true`, QDF takes precedence over every `stream_data`
    /// value (including `Preserve`) and forces decoded output.
    ///
    /// **Note:** JSON output paths (`json_inspect`) are not yet wired to this field;
    /// only the full-rewrite path is affected (tracked separately).
    ///
    /// [`compress_streams`]: WriteOptions::compress_streams
    pub stream_data: Option<StreamDataMode>,

    /// Re-encode streams that are already a lone `/FlateDecode`.
    ///
    /// By default (`false`) a stream whose source filter is a single
    /// `/FlateDecode` is emitted **verbatim** under [`CompressStreams::Yes`] —
    /// its already-compressed bytes are preserved rather than decoded and
    /// re-encoded. This mirrors qpdf, which does not recompress a lone-Flate
    /// stream unless `--recompress-flate` is given.
    ///
    /// Set to `true` to force such streams through a decode + re-encode pass
    /// (equivalent to `qpdf --recompress-flate`). Has no effect under
    /// [`CompressStreams::No`] / [`StreamDataMode::Uncompress`] (which always
    /// decode) or [`StreamDataMode::Preserve`] (which never decodes).
    ///
    /// A lone-Flate stream that carries an external-file reference (`/F`) is
    /// always re-encoded regardless of this flag: its in-body bytes are not the
    /// canonical data, so they are never preserved verbatim.
    pub recompress_flate: bool,

    /// Encrypt the output with the supplied [`crate::EncryptParams`] (qpdf
    /// `--encrypt …` equivalent).
    ///
    /// When set the writer:
    ///
    /// 1. Resolves `/ID[0]` upfront (preserving the input's permanent
    ///    identifier when present, generating a fresh one otherwise) so
    ///    Algorithm 2 can derive the file encryption key from it.
    /// 2. Builds the `/Encrypt` dictionary via the algorithm-specific
    ///    builder (`build_v4_encrypt_dict` for the V=4 AES-128 walking
    ///    skeleton).
    /// 3. Encrypts every string in every emitted object (per-object key
    ///    via Algorithm 1) and every stream payload (with random AES IV
    ///    prepended + PKCS#7 padding, `/Length` updated to match).
    /// 4. Emits the `/Encrypt` dictionary itself as a plaintext indirect
    ///    object whose number is referenced from the trailer.
    ///
    /// **Required flag combinations** (the writer currently rejects others):
    ///
    /// - `full_rewrite` is implicitly forced to `true` (the incremental
    ///   path cannot rewrite source object bytes).
    /// - `qdf` must be `false` (QDF mode emits plaintext for human
    ///   inspection; the combination is rejected with `Unsupported`).
    pub encrypt: Option<crate::encrypt_setup::EncryptParams>,

    /// Copy the `/Encrypt` dictionary verbatim from a donor PDF and re-use its
    /// file encryption key (qpdf `--copy-encryption-from` equivalent).
    ///
    /// When set the writer bypasses the normal password-derivation path and
    /// constructs an `EncryptionContext` directly from the pre-recovered file
    /// key, the donor's `/Encrypt` dict, and the donor's `/ID[0]`.  The output
    /// can therefore be decrypted with the donor's user and owner passwords.
    ///
    /// Exactly one of `encrypt` and `copy_encryption` may be set; the CLI
    /// enforces mutual exclusion via `conflicts_with`.  The writer asserts this
    /// invariant at the top of the full-rewrite path.
    ///
    /// **Scope:** Only V=4 AES-128 donors are currently supported; other schemes
    /// are rejected by the CLI before this field is populated.
    pub copy_encryption: Option<crate::encrypt_setup::CopyEncryptionSource>,
}

/// Parse a PDF version string of the form `"M.m"` into `(major, minor)`.
///
/// Returns `None` for any string that does not match `digit+ '.' digit+`.
/// Only `1.x` documents are common in practice; `2.0` uses the same syntax.
pub fn parse_pdf_version(v: &str) -> Option<(u8, u8)> {
    let (major, minor) = v.split_once('.')?;
    let major: u8 = major.parse().ok()?;
    let minor: u8 = minor.parse().ok()?;
    Some((major, minor))
}

/// Compute the effective PDF version to write given the source version, the
/// caller-supplied options, and whether the output is linearized.
///
/// Rule (mirrors qpdf):
/// 1. If `options.force_version` is set, use it verbatim.
/// 2. Otherwise start from `max(source, min_version_option)`.
/// 3. If `object_streams` is true, apply a `max(…, "1.5")` floor. Cross-
///    reference and object streams were introduced in PDF 1.5, so the output
///    must use at least 1.5 whenever such streams are actually emitted. The
///    caller passes whether the output *really* contains an object stream (not
///    merely whether the mode requests it), so a generate request that packs
///    nothing leaves the version untouched, matching qpdf.
/// 4. If `linearize` is true, apply an additional `max(…, "1.2")` floor
///    (linearized PDFs require at least PDF 1.2).
///
/// If the version strings cannot be parsed the function falls back to the
/// `source` string unchanged (rather than panicking) so callers do not need to
/// validate before calling.
///
/// # `/Catalog /Version` reconciliation (qpdf semantics)
///
/// ISO 32000-1 §7.5.2 lets a `/Catalog /Version` entry override the header
/// when it is *higher*; readers compute the effective version as
/// `max(header, catalog)`. Empirically (verified against qpdf 11.x with
/// `qpdf --force-version` / `--min-version` on fixtures carrying a
/// `/Catalog /Version`), qpdf rewrites **only** the `%PDF-x.y` header line and
/// never strips, lowers, or otherwise touches `/Catalog /Version` — even when
/// it is higher than the chosen header. It also does **not** fold
/// `/Catalog /Version` into the source floor: the `--min-version` baseline is
/// the header version alone, not `max(header, catalog)`.
///
/// "Reconciled per qpdf semantics" therefore means *leave `/Catalog /Version`
/// alone* — `source` here is the header version and this function deliberately
/// does not read the Catalog. This keeps the implementation minimal and
/// byte-faithful to qpdf rather than guessing at a broader reconciliation.
pub fn effective_pdf_version<'a>(
    source: &'a str,
    options: &'a WriteOptions,
    linearize: bool,
    object_streams: bool,
) -> &'a str {
    // --force-version wins outright, but only when the value is a valid version string.
    // Silently ignore invalid values (same treatment as invalid min_version) so that
    // callers that cannot pre-validate do not produce a corrupted PDF header.
    if let Some(ref forced) = options.force_version {
        if parse_pdf_version(forced).is_some() {
            return forced.as_str();
        }
    }

    // Parse source; bail to source string on failure.
    let Some(mut best) = parse_pdf_version(source) else {
        return source;
    };

    // Apply --min-version floor.
    if let Some(ref min_v) = options.min_version {
        if let Some(min_parsed) = parse_pdf_version(min_v) {
            if min_parsed > best {
                best = min_parsed;
            }
        }
    }

    // Apply object-stream floor (object streams require >= 1.5).
    if object_streams {
        let objstm_floor = (1u8, 5u8);
        if objstm_floor > best {
            best = objstm_floor;
        }
    }

    // Apply linearize floor (PDF spec requires >= 1.2).
    if linearize {
        let lin_floor = (1u8, 2u8);
        if lin_floor > best {
            best = lin_floor;
        }
    }

    // If best == source parsed, return the original source slice to avoid an
    // allocation.  Otherwise find which option string owns this version.
    if parse_pdf_version(source) == Some(best) {
        return source;
    }
    if let Some(ref min_v) = options.min_version {
        if parse_pdf_version(min_v) == Some(best) {
            return min_v.as_str();
        }
    }
    // Object-stream floor "1.5" — reached when best == (1,5) and neither source
    // nor min_version matched (object streams forced the version up).
    if best == (1u8, 5u8) {
        return "1.5";
    }
    // Linearize floor "1.2" — only reached when best == (1,2) and neither
    // source nor min_version matched.
    "1.2"
}

/// Binary header marker emitted by qpdf on the second line of every output
/// PDF (immediately after the `%PDF-x.y` version line).  The four bytes are
/// all > 127, which signals to file-transfer tools that the file is binary,
/// as recommended by the PDF specification.  We fix these to qpdf's values so
/// that flpdf output is byte-identical to qpdf output for the header section.
///
/// Hex: `25 BF F7 A2 FE 0A`  →  `%` + four high bytes + newline.
///
/// Shared with the linearization writer ([`crate::linearization`]) so the
/// linearized output uses the identical marker as the plain rewrite path.
pub(crate) const QPDF_BINARY_MARKER: &[u8] = b"%\xbf\xf7\xa2\xfe\n";

/// qpdf's static-id constant: the first 32 hex digits of π, encoded as 16 raw
/// bytes so the trailer emits `<31415926535897932384626433832795>`.
pub(crate) const QPDF_STATIC_ID: [u8; 16] = [
    0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93, 0x23, 0x84, 0x62, 0x64, 0x33, 0x83, 0x27, 0x95,
];

/// Write `pdf` as an incrementally-updated revision (qpdf's default `--object-streams=preserve` mode).
///
/// The original bytes are copied to `out` unchanged, then a single update section is
/// appended for every object that has been mutated via [`Pdf::set_object`]. The xref
/// table form (table vs. stream) is preserved from the input, so the output remains
/// readable by older PDF consumers when the source used `xref` tables and stays
/// compact when the source used cross-reference streams.
///
/// # Errors
///
/// - [`crate::Error::Missing`] if the input has no `/Root`.
/// - Propagates I/O errors and structural PDF errors from the underlying
///   incremental writer (this path uses [`WriteOptions::default`], so it never
///   takes the full-rewrite, encryption, or QDF branches).
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{write_pdf, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let mut out = File::create("output.pdf")?;
/// write_pdf(&mut pdf, &mut out)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, out: W) -> Result<()> {
    write_pdf_with_options(pdf, out, &WriteOptions::default())
}

/// Like [`write_pdf`] but with caller-supplied [`WriteOptions`].
///
/// # Errors
///
/// - [`crate::Error::Missing`] if the input has no `/Root`.
/// - [`crate::Error::Signed`] when the full-rewrite path
///   (`options.full_rewrite == true`) would invalidate existing signatures and
///   `options.allow_signed_full_rewrite` is not set.
/// - [`crate::Error::Unsupported`] when `options.encrypt` and
///   `options.copy_encryption` are both set (mutually exclusive), when
///   encryption is combined with `options.qdf`, or when the OS CSPRNG
///   (`getrandom`) is unavailable while deriving encryption keys or an AES IV.
/// - [`crate::Error::Encrypted`] when `options.encrypt` or
///   `options.copy_encryption` is set but the requested encryption parameters
///   cannot be realized (an unsupported handler combination or malformed donor
///   `/Encrypt` data).
/// - Propagates I/O errors and structural PDF errors from the underlying
///   incremental or full-rewrite writer.
pub fn write_pdf_with_options<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriteOptions,
) -> Result<()> {
    if options.full_rewrite {
        return write_pdf_full_rewrite(pdf, out, options);
    }
    if options.deterministic_id {
        // The deterministic /ID is an MD5 over the rewritten body, which only
        // the full-rewrite path produces. Reject rather than silently emit a
        // random /ID and break the "deterministic" contract.
        return Err(crate::Error::Unsupported(
            "deterministic-id requires a full rewrite".to_string(),
        ));
    }
    write_pdf_incremental(pdf, out, options)
}

/// Bookkeeping for a Generate-mode incremental ObjStm container: its
/// allocated reference, its byte offset within the appended section (a
/// type-1 / plain xref entry), and the `member number -> (container number,
/// index)` map used to emit type-2 (compressed) xref-stream entries.
struct ObjStmIncremental {
    container: ObjectRef,
    container_offset: usize,
    /// member number -> (container number, index)
    compressed: BTreeMap<u32, (u32, u32)>,
}

fn write_pdf_incremental<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut bytes = pdf.source_bytes()?;
    // flpdf-9hc.22.4: the incremental path only ever *appends* to the source
    // bytes, so the original prefix — and therefore any signed `/ByteRange`
    // region within it — stays bit-identical. In debug builds capture the
    // source length and an MD5 digest of it (O(1) memory, instead of an O(N)
    // clone of a possibly hundreds-of-MB PDF) so the invariant can be asserted
    // before we hand off the buffer.
    #[cfg(debug_assertions)]
    let (source_len, source_digest) = {
        use md5::Digest as _;
        let mut hasher = md5::Md5::new();
        hasher.update(&bytes);
        (bytes.len(), hasher.finalize())
    };
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }

    let source_offsets = build_source_offsets(pdf.source_xref_offsets());
    let source_xref_offsets = build_source_xref_offsets(pdf.source_xref_entries());
    let (touched_object_refs, deleted_object_refs, touched_objstm_members) =
        collect_touched_object_refs(pdf);

    // flpdf-9hc.5.9 Task 5: Generate-mode incremental ObjStm packing. The gate
    // is exactly (Generate mode) AND (source last xref is a stream) AND
    // (non-empty packable). If any condition fails the path is byte-identical
    // to plain incremental: `plain_touched == touched_object_refs` and
    // `objstm_inc` stays `None`. `touched_objstm_members` (existing-ObjStm
    // members) and `deleted_object_refs` are untouched and continue through
    // their existing paths.
    let empty_compressed: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    let use_objstm = options.object_streams == ObjectStreamMode::Generate
        && matches!(pdf.last_xref_form(), XrefForm::Stream);

    let mut objstm_inc: Option<ObjStmIncremental> = None;
    let plain_touched: Vec<ObjectRef> = if use_objstm {
        let (packable, plain_remaining) = partition_objstm_eligible(pdf, &touched_object_refs)?;
        if packable.is_empty() {
            touched_object_refs.clone()
        } else {
            let declared =
                resolve_xref_stream_object_count(pdf.trailer().get("Size"), &source_xref_offsets);
            let container = allocate_incremental_objstm_container(
                &source_xref_offsets,
                &touched_object_refs,
                &deleted_object_refs,
                declared,
            )?;
            let (container_offset, members) =
                write_incremental_objstm(&mut bytes, pdf, container, &packable, options)?;
            let mut compressed = BTreeMap::new();
            for (r, idx) in members {
                compressed.insert(r.number, (container.number, idx));
            }
            objstm_inc = Some(ObjStmIncremental {
                container,
                container_offset,
                compressed,
            });
            plain_remaining
        }
    } else {
        touched_object_refs.clone()
    };

    let mut xref_offsets = write_incremental_objects(&mut bytes, pdf, &plain_touched)?;
    if let Some(oi) = &objstm_inc {
        // The container is a plain (type-1) indirect object in the appended
        // section; its byte offset was captured before any plain object.
        xref_offsets.insert(oi.container.number, (0, oi.container_offset));
    }
    let deleted_table_entries = build_deleted_table_entries(pdf, &deleted_object_refs);
    let rewritten_stream_offsets =
        write_updated_object_streams(&mut bytes, pdf, &touched_objstm_members)?;
    xref_offsets.extend(rewritten_stream_offsets);
    let final_offsets =
        merge_source_and_touched_offsets(&source_offsets, &xref_offsets, &deleted_table_entries);
    let final_xref_offsets = merge_source_and_touched_offsets_for_xref_stream(
        &source_xref_offsets,
        &xref_offsets,
        &deleted_object_refs,
        objstm_inc
            .as_ref()
            .map(|o| &o.compressed)
            .unwrap_or(&empty_compressed),
    );
    let mut object_count = match pdf.last_xref_form() {
        XrefForm::Table => resolve_object_count(pdf.trailer().get("Size"), &final_offsets),
        XrefForm::Stream => {
            resolve_xref_stream_object_count(pdf.trailer().get("Size"), &final_xref_offsets)
        }
    };
    if let Some(oi) = &objstm_inc {
        object_count = object_count.max(oi.container.number as usize + 1);
    }

    let xref_offset = match pdf.last_xref_form() {
        XrefForm::Table => write_incremental_xref(&mut bytes, &final_offsets)?,
        XrefForm::Stream => {
            let xref_object_number = next_xref_stream_object_number(&final_xref_offsets)?;
            object_count = object_count.max(xref_object_number as usize + 1);
            let xref_offset = write_incremental_xref_stream(
                &mut bytes,
                pdf.trailer(),
                &final_xref_offsets,
                &root_ref,
                xref_object_number,
                object_count,
                pdf.previous_xref_offset(),
            )?;
            xref_offset
        }
    };
    write_incremental_trailer(
        &mut bytes,
        pdf,
        &root_ref,
        object_count,
        pdf.previous_xref_offset(),
        xref_offset,
        options,
    )?;

    // flpdf-9hc.22.4: guard the byte-preservation invariant. The source prefix
    // (and any signed `/ByteRange` covered by it) must survive verbatim — the
    // appended trailing `\n` lands *after* it, so we re-hash exactly the first
    // `source_len` bytes (not the whole buffer) and compare against the digest
    // captured up front. `.get(..source_len)` keeps a pathological shrink from
    // panicking the index, surfacing it as a clean assertion failure instead.
    #[cfg(debug_assertions)]
    {
        use md5::Digest as _;
        let mut hasher = md5::Md5::new();
        hasher.update(bytes.get(..source_len).unwrap_or(&bytes));
        debug_assert_eq!(
            hasher.finalize(),
            source_digest,
            "incremental update must preserve the original source prefix byte-for-byte"
        );
    }

    out.write_all(&bytes)?;
    Ok(())
}

type RewrittenObjStmMembers = BTreeMap<ObjectRef, BTreeSet<ObjectRef>>;

fn collect_touched_object_refs<R: Read + Seek>(
    pdf: &Pdf<R>,
) -> (Vec<ObjectRef>, Vec<ObjectRef>, RewrittenObjStmMembers) {
    let mut touched = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    let mut objstm_touched: BTreeMap<ObjectRef, BTreeSet<ObjectRef>> = BTreeMap::new();

    for object_ref in pdf.deleted_object_refs() {
        deleted.insert(object_ref);
    }

    for object_ref in pdf.dirty_object_refs() {
        if deleted.contains(&object_ref) {
            continue;
        }
        if let Some((stream_ref, _index)) = pdf.compressed_parent(object_ref) {
            objstm_touched
                .entry(stream_ref)
                .or_default()
                .insert(object_ref);
            continue;
        }

        touched.insert(object_ref);
    }

    (
        touched.into_iter().collect(),
        deleted.into_iter().collect(),
        objstm_touched,
    )
}

/// Split `touched` into (objstm_packable, plain_remaining) using the same
/// eligibility predicate as the full-rewrite ObjStm packer. Order-preserving.
///
/// Wired into the incremental Generate-mode gate by `write_pdf_incremental`.
fn partition_objstm_eligible<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    touched: &[ObjectRef],
) -> Result<(Vec<ObjectRef>, Vec<ObjectRef>)> {
    let ctx = object_streams::eligibility_context(pdf)?;
    let mut packable = Vec::new();
    let mut plain = Vec::new();
    for &r in touched {
        let obj = pdf.resolve_borrowed(r)?;
        if object_streams::is_eligible_for_objstm(r, obj, &ctx) {
            packable.push(r);
        } else {
            plain.push(r);
        }
    }
    Ok((packable, plain))
}

#[derive(Clone, Copy)]
enum XrefTableEntry {
    InUse { generation: u16, offset: usize },
    Free { generation: u16, next: u32 },
}

fn write_incremental_objects<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    touched_object_refs: &[ObjectRef],
) -> Result<BTreeMap<u32, (u16, usize)>> {
    let mut updated_offsets = BTreeMap::new();

    for object_ref in touched_object_refs {
        let object = pdf.resolve_borrowed(*object_ref)?;
        let Some(offset) = write_object(bytes, *object_ref, object)? else {
            continue;
        };
        updated_offsets.insert(object_ref.number, (object_ref.generation, offset));
    }

    Ok(updated_offsets)
}

fn write_updated_object_streams<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    members_by_stream: &RewrittenObjStmMembers,
) -> Result<BTreeMap<u32, (u16, usize)>> {
    let mut updated_offsets = BTreeMap::new();

    for (stream_ref, member_refs) in members_by_stream {
        let stream_object = pdf.resolve(*stream_ref)?;
        let Some(stream) = stream_object.into_stream() else {
            return Err(crate::Error::Unsupported(format!(
                "object {} is not an object stream",
                stream_ref.number
            )));
        };

        let (rebuilt_data, rebuilt_first) = rebuild_object_stream(&stream, member_refs, pdf)?;

        let mut stream_dict = stream.dict.clone();
        stream_dict.insert(
            "First",
            Object::Integer(i64::try_from(rebuilt_first).map_err(|_| {
                crate::Error::Unsupported("object stream /First too large".to_string())
            })?),
        );
        stream_dict.insert(
            "Length",
            Object::Integer(i64::try_from(rebuilt_data.len()).map_err(|_| {
                crate::Error::Unsupported("object stream /Length too large".to_string())
            })?),
        );

        let rebuilt_stream = Object::Stream(crate::Stream::new(stream_dict, rebuilt_data));
        let offset = write_object(bytes, *stream_ref, &rebuilt_stream)?
            .expect("writing object always returns an offset");
        updated_offsets.insert(stream_ref.number, (stream_ref.generation, offset));
    }

    Ok(updated_offsets)
}

fn rebuild_object_stream<R: Read + Seek>(
    stream: &crate::Stream,
    updated_members: &BTreeSet<ObjectRef>,
    pdf: &mut Pdf<R>,
) -> Result<(Vec<u8>, usize)> {
    let stream_data = filters::decode_stream_data(&stream.dict, &stream.data)?;

    let object_count = usize::try_from(parse_non_negative_i64(
        stream
            .dict
            .get("N")
            .ok_or(crate::Error::Missing("Object stream /N"))?,
        "Object stream /N",
    )?)
    .map_err(|_| crate::Error::Unsupported("Object stream /N does not fit usize".to_string()))?;

    let first = usize::try_from(parse_non_negative_i64(
        stream
            .dict
            .get("First")
            .ok_or(crate::Error::Missing("Object stream /First"))?,
        "Object stream /First",
    )?)
    .map_err(|_| {
        crate::Error::Unsupported("Object stream /First does not fit usize".to_string())
    })?;

    let mut header_parser = Parser::new(&stream_data);
    let mut members = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        let number = u32::try_from(parse_non_negative_i64_value(
            header_parser.integer_for_indirect()?,
            "object stream object number",
        )?)
        .map_err(|_| {
            crate::Error::Unsupported("object stream object number does not fit u32".to_string())
        })?;
        let offset = usize::try_from(parse_non_negative_i64_value(
            header_parser.integer_for_indirect()?,
            "object stream object offset",
        )?)
        .map_err(|_| {
            crate::Error::Unsupported("object stream object offset does not fit usize".to_string())
        })?;
        members.push((number, offset));
    }

    let mut decoded_members = Vec::with_capacity(object_count);
    for (index, (number, offset)) in members.iter().enumerate() {
        let start = first.checked_add(*offset).ok_or_else(|| {
            crate::Error::Unsupported("object stream offset overflow".to_string())
        })?;
        let end = if index + 1 < members.len() {
            first.checked_add(members[index + 1].1).ok_or_else(|| {
                crate::Error::Unsupported("object stream offset overflow".to_string())
            })?
        } else {
            stream_data.len()
        };

        if start > end || end > stream_data.len() {
            return Err(crate::Error::parse(
                0,
                "compressed object offset out of range",
            ));
        }

        let object_ref = ObjectRef::new(*number, 0);
        let object = if updated_members.contains(&object_ref) {
            pdf.resolve(object_ref)?
        } else {
            let mut parser = Parser::new(&stream_data[start..end]);
            parser.object()?
        };

        decoded_members.push((object_ref, object));
    }

    let mut header = Vec::new();
    let mut body = Vec::new();
    let mut object_offsets = Vec::with_capacity(decoded_members.len());

    for (_, object) in &decoded_members {
        object_offsets.push(body.len());
        object.write_pdf(&mut body);
        body.push(b'\n');
    }

    for ((object_ref, _), offset) in decoded_members.iter().zip(object_offsets.iter()) {
        header.extend_from_slice(format!("{} {} ", object_ref.number, offset).as_bytes());
    }

    let rebuilt_first = header.len();

    let mut rebuilt_data = header;
    rebuilt_data.extend_from_slice(&body);

    let encoded = filters::encode_stream_data(&stream.dict, &rebuilt_data)?;
    Ok((encoded, rebuilt_first))
}

fn parse_non_negative_i64(value: &crate::Object, context: &str) -> Result<i64> {
    let crate::Object::Integer(integer) = value else {
        return Err(crate::Error::parse(0, format!("{context} is not integer")));
    };
    if *integer < 0 {
        return Err(crate::Error::parse(0, format!("{context} is negative")));
    }
    Ok(*integer)
}

fn parse_non_negative_i64_value(value: i64, context: &str) -> Result<i64> {
    if value < 0 {
        return Err(crate::Error::parse(0, format!("{context} is negative")));
    }
    Ok(value)
}

fn write_object(
    bytes: &mut Vec<u8>,
    object_ref: ObjectRef,
    object: &Object,
) -> Result<Option<usize>> {
    let offset = bytes.len();
    bytes.extend_from_slice(
        format!("{} {} obj\n", object_ref.number, object_ref.generation).as_bytes(),
    );
    object.write_pdf(bytes);
    bytes.extend_from_slice(b"\nendobj\n");

    Ok(Some(offset))
}

fn merge_source_and_touched_offsets(
    source_offsets: &BTreeMap<u32, (u16, usize)>,
    touched_offsets: &BTreeMap<u32, (u16, usize)>,
    deleted_entries: &BTreeMap<u32, (u16, u32)>,
) -> BTreeMap<u32, XrefTableEntry> {
    let mut merged = source_offsets
        .iter()
        .map(|(number, (generation, offset))| {
            (
                *number,
                XrefTableEntry::InUse {
                    generation: *generation,
                    offset: *offset,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (number, (generation, offset)) in touched_offsets {
        merged.insert(
            *number,
            XrefTableEntry::InUse {
                generation: *generation,
                offset: *offset,
            },
        );
    }
    for (number, (generation, next)) in deleted_entries {
        merged.insert(
            *number,
            XrefTableEntry::Free {
                generation: *generation,
                next: *next,
            },
        );
    }
    merged
}

fn merge_source_and_touched_offsets_for_xref_stream(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    touched_offsets: &BTreeMap<u32, (u16, usize)>,
    deleted_object_refs: &[ObjectRef],
    compressed_members: &BTreeMap<u32, (u32, u32)>,
) -> BTreeMap<u32, (u16, XrefOffset)> {
    let mut merged = source_offsets.clone();
    for (number, (generation, offset)) in touched_offsets {
        merged.insert(*number, (*generation, XrefOffset::Offset(*offset as u64)));
    }
    for (number, (stream, index)) in compressed_members {
        merged.insert(
            *number,
            (
                // Type-2 (compressed) xref entries require the object's generation to be 0 (ISO 32000-1 §7.5.8.3); the third field carries the ObjStm index, not a generation.
                0,
                XrefOffset::Compressed {
                    stream: *stream,
                    index: *index,
                },
            ),
        );
    }
    for (number, (generation, next)) in build_deleted_entries(source_offsets, deleted_object_refs) {
        merged.insert(number, (generation, XrefOffset::Free { next }));
    }
    merged
}

fn build_deleted_table_entries<R: Read + Seek>(
    pdf: &Pdf<R>,
    deleted_object_refs: &[ObjectRef],
) -> BTreeMap<u32, (u16, u32)> {
    let source_offsets = build_source_xref_offsets(pdf.source_xref_entries());
    build_deleted_entries(&source_offsets, deleted_object_refs)
}

fn build_deleted_entries(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    deleted_object_refs: &[ObjectRef],
) -> BTreeMap<u32, (u16, u32)> {
    if deleted_object_refs.is_empty() {
        return BTreeMap::new();
    }

    let mut deleted_refs = deleted_object_refs.to_vec();
    deleted_refs.sort_by_key(|object_ref| object_ref.number);
    deleted_refs.dedup_by_key(|object_ref| object_ref.number);

    let mut entries = BTreeMap::new();
    let first_deleted = deleted_refs[0].number;
    entries.insert(0, (65535, first_deleted));

    deleted_refs
        .iter()
        .enumerate()
        .for_each(|(index, object_ref)| {
            let next = deleted_refs
                .get(index + 1)
                .map(|next_ref| next_ref.number)
                .unwrap_or_else(|| source_free_head(source_offsets));
            entries.insert(
                object_ref.number,
                (
                    incremented_generation(current_generation(source_offsets, *object_ref)),
                    next,
                ),
            );
        });

    entries
}

fn source_free_head(source_offsets: &BTreeMap<u32, (u16, XrefOffset)>) -> u32 {
    match source_offsets.get(&0) {
        Some((_, XrefOffset::Free { next })) => *next,
        _ => 0,
    }
}

fn current_generation(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    object_ref: ObjectRef,
) -> u16 {
    source_offsets
        .get(&object_ref.number)
        .map(|(generation, _)| *generation)
        .unwrap_or(object_ref.generation)
}

fn incremented_generation(generation: u16) -> u16 {
    generation.saturating_add(1)
}

fn next_xref_stream_object_number(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
) -> Result<u32> {
    source_offsets
        .keys()
        .copied()
        .next_back()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("xref object number does not fit u32".to_string()))
}

/// Allocate a fresh ObjStm container number strictly above the existing input
/// space (source xref max, touched, deleted) so it never collides with a
/// `delete_object` free entry.
///
/// Wired into the incremental write path by `write_pdf_incremental`.
fn allocate_incremental_objstm_container(
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    touched: &[ObjectRef],
    deleted: &[ObjectRef],
    declared_size: usize,
) -> Result<ObjectRef> {
    let max_source = source_offsets.keys().copied().next_back().unwrap_or(0);
    let max_touched = touched.iter().map(|r| r.number).max().unwrap_or(0);
    let max_deleted = deleted.iter().map(|r| r.number).max().unwrap_or(0);
    let declared_u32 = u32::try_from(declared_size.saturating_sub(1))
        .map_err(|_| crate::Error::Unsupported("declared /Size does not fit u32".to_string()))?;
    let base = max_source
        .max(max_touched)
        .max(max_deleted)
        .max(declared_u32);
    let number = base.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported("ObjStm container number does not fit u32".to_string())
    })?;
    Ok(ObjectRef::new(number, 0))
}

/// Emit the incremental-update ObjStm container object into `bytes` and return
/// its byte offset together with the `(member, index-within-container)` pairs.
///
/// The container body and stream wrapper are produced by the exact same helpers
/// the full-rewrite path uses ([`object_streams::emit_objstm_body`] /
/// [`object_streams::wrap_objstm_body`]); the compress policy mirrors the
/// full-rewrite call site verbatim (`options.compress_streams`).  The returned
/// member map is consumed by `write_pdf_incremental`
/// to build type-2 (compressed) xref entries via
/// `merge_source_and_touched_offsets_for_xref_stream`'s `compressed_members`
/// parameter.
///
/// Container framing is emitted via `write_object` (single unconditional `\n`
/// before `endstream`); unlike the full-rewrite ObjStm path it does not
/// consult `options.newline_before_endstream`. The `incremental_generate_qpdf_check`
/// cross-check confirms `qpdf --check` reports no delta under the default
/// `NewlineBeforeEndstream::Yes`; a divergence is only possible under
/// `NewlineBeforeEndstream::No` with a payload ending in `\n`/`\r`.
///
/// Wired into the incremental write path by `write_pdf_incremental`.
fn write_incremental_objstm<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    container_ref: ObjectRef,
    packable: &[ObjectRef],
    options: &WriteOptions,
) -> Result<(usize, Vec<(ObjectRef, u32)>)> {
    let body = object_streams::emit_objstm_body(pdf, packable)?;
    let stream = object_streams::wrap_objstm_body(&body, options.compress_streams)?;
    let offset = bytes.len();
    write_object(bytes, container_ref, &Object::Stream(stream))?;
    let members = packable
        .iter()
        .enumerate()
        .map(|(i, &r)| (r, i as u32))
        .collect();
    Ok((offset, members))
}

fn build_source_xref_offsets(
    source_offsets: BTreeMap<ObjectRef, XrefOffset>,
) -> BTreeMap<u32, (u16, XrefOffset)> {
    let mut offsets = BTreeMap::new();
    for (object_ref, xref_offset) in source_offsets {
        offsets.insert(object_ref.number, (object_ref.generation, xref_offset));
    }
    offsets
}

fn build_source_offsets(entries: Vec<(ObjectRef, u64)>) -> BTreeMap<u32, (u16, usize)> {
    let mut source_offsets = BTreeMap::new();

    for (object_ref, xref_offset) in entries {
        let next = source_offsets
            .get(&object_ref.number)
            .copied()
            .map(|(generation, _)| generation)
            .unwrap_or(0);

        if object_ref.generation >= next {
            source_offsets.insert(
                object_ref.number,
                (object_ref.generation, xref_offset as usize),
            );
        }
    }

    source_offsets
}

fn resolve_object_count(
    declared_size: Option<&crate::Object>,
    source_offsets: &BTreeMap<u32, XrefTableEntry>,
) -> usize {
    let max_object_number = source_offsets.keys().next_back().copied().unwrap_or(0) as usize;
    let declared = declared_size
        .and_then(|size| match size {
            crate::Object::Integer(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(0);

    declared.max(max_object_number.saturating_add(1)).max(1)
}

fn resolve_xref_stream_object_count(
    declared_size: Option<&crate::Object>,
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
) -> usize {
    let max_object_number = source_offsets.keys().next_back().copied().unwrap_or(0) as usize;
    let declared = declared_size
        .and_then(|size| match size {
            crate::Object::Integer(value) => usize::try_from(*value).ok(),
            _ => None,
        })
        .unwrap_or(0);

    declared.max(max_object_number.saturating_add(1)).max(1)
}

fn write_incremental_xref(
    bytes: &mut Vec<u8>,
    source_offsets: &BTreeMap<u32, XrefTableEntry>,
) -> Result<usize> {
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 1\n");
    let (free_generation, free_next) = match source_offsets.get(&0) {
        Some(XrefTableEntry::Free { generation, next }) => (*generation, *next),
        _ => (65535, 0),
    };
    bytes.extend_from_slice(format!("{free_next:010} {free_generation:05} f \n").as_bytes());

    let mut object_numbers: Vec<u32> = source_offsets.keys().copied().filter(|n| *n > 0).collect();
    if object_numbers.is_empty() {
        return Ok(xref_offset);
    }

    object_numbers.sort_unstable();

    let mut i = 0;
    while i < object_numbers.len() {
        let start = object_numbers[i];
        let mut end = start;

        while i + 1 < object_numbers.len() && object_numbers[i + 1] == end + 1 {
            i += 1;
            end = object_numbers[i];
        }

        let count = end - start + 1;
        bytes.extend_from_slice(format!("{} {}\n", start, count).as_bytes());
        for object_number in start..=end {
            let entry = source_offsets.get(&object_number).ok_or_else(|| {
                crate::Error::Unsupported(
                    "incremental xref subsection is missing object entry".to_string(),
                )
            })?;

            match entry {
                XrefTableEntry::InUse { generation, offset } => {
                    bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes())
                }
                XrefTableEntry::Free { generation, next } => {
                    bytes.extend_from_slice(format!("{next:010} {generation:05} f \n").as_bytes())
                }
            }
        }

        i += 1;
    }

    Ok(xref_offset)
}

fn write_incremental_xref_stream(
    bytes: &mut Vec<u8>,
    trailer: &Dictionary,
    source_offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    root_ref: &ObjectRef,
    xref_object_number: u32,
    object_count: usize,
    previous_xref_offset: u64,
) -> Result<usize> {
    let xref_offset = bytes.len();
    if source_offsets.contains_key(&xref_object_number) {
        return Err(crate::Error::Unsupported(format!(
            "xref stream object number {} already exists",
            xref_object_number
        )));
    }

    let mut offsets = source_offsets.clone();
    offsets.insert(0, (65535, XrefOffset::Free { next: 0 }));
    offsets.insert(
        xref_object_number,
        (
            0,
            XrefOffset::Offset(u64::try_from(xref_offset).map_err(|_| {
                crate::Error::Unsupported("xref stream object offset does not fit u64".to_string())
            })?),
        ),
    );

    let mut object_numbers: Vec<u32> = offsets.keys().copied().collect();
    object_numbers.sort_unstable();

    let mut ranges = Vec::new();
    let mut index = 0;
    while index < object_numbers.len() {
        let start = object_numbers[index];
        let mut end = start;
        while index + 1 < object_numbers.len() && object_numbers[index + 1] == end + 1 {
            index += 1;
            end = object_numbers[index];
        }

        ranges.push((start, end.saturating_sub(start).saturating_add(1)));
        index += 1;
    }

    let stream_data = build_xref_stream_bytes(&offsets, &ranges)?;

    let mut index_array = Vec::with_capacity(ranges.len() * 2);
    for (start, count) in &ranges {
        index_array.push(Object::Integer(i64::from(*start)));
        index_array.push(Object::Integer(i64::from(*count)));
    }

    let size = i64::try_from(object_count)
        .map_err(|_| crate::Error::Unsupported("xref stream /Size does not fit i64".to_string()))?;
    let mut stream_dict = trailer.clone();
    strip_incremental_trailer_keys(&mut stream_dict);
    stream_dict.insert("Type", Object::Name(b"XRef".to_vec()));
    stream_dict.insert("Size", Object::Integer(size));
    stream_dict.insert(
        "W",
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(8),
            Object::Integer(4),
        ]),
    );
    stream_dict.insert("Index", Object::Array(index_array));
    stream_dict.insert("Root", Object::Reference(*root_ref));
    stream_dict.insert(
        "Length",
        Object::Integer(i64::try_from(stream_data.len()).map_err(|_| {
            crate::Error::Unsupported("xref stream /Length does not fit i64".to_string())
        })?),
    );
    stream_dict.insert(
        "Prev",
        Object::Integer(previous_xref_offset.try_into().map_err(|_| {
            crate::Error::Unsupported("startxref offset does not fit i64".to_string())
        })?),
    );

    let xref_stream = Object::Stream(crate::Stream::new(stream_dict, stream_data));
    write_object(bytes, ObjectRef::new(xref_object_number, 0), &xref_stream)?;

    Ok(xref_offset)
}

fn build_xref_stream_bytes(
    offsets: &BTreeMap<u32, (u16, XrefOffset)>,
    ranges: &[(u32, u32)],
) -> Result<Vec<u8>> {
    let mut stream_data = Vec::new();
    for &(start, count) in ranges {
        let end = start.checked_add(count).ok_or_else(|| {
            crate::Error::Unsupported("xref stream range does not fit u32".to_string())
        })?;
        for object_number in start..end {
            let (generation, xref_offset) = offsets.get(&object_number).ok_or_else(|| {
                crate::Error::Unsupported(
                    "incremental xref stream is missing object entry".to_string(),
                )
            })?;

            let object_type = match (object_number, xref_offset) {
                (0, _) | (_, XrefOffset::Free { .. }) => 0,
                (_, XrefOffset::Compressed { .. }) => 2,
                _ => 1,
            };

            stream_data.push(object_type);
            match xref_offset {
                XrefOffset::Free { next } => {
                    stream_data.extend_from_slice(&(u64::from(*next).to_be_bytes()[0..8]));
                    stream_data.extend_from_slice(&u32::from(*generation).to_be_bytes()[0..4]);
                }
                XrefOffset::Offset(offset) => {
                    stream_data.extend_from_slice(&((*offset).to_be_bytes()[0..8]));
                    stream_data.extend_from_slice(&u32::from(*generation).to_be_bytes()[0..4]);
                }
                XrefOffset::Compressed { stream, index } => {
                    stream_data.extend_from_slice(&(u64::from(*stream).to_be_bytes()[0..8]));
                    stream_data.extend_from_slice(&u32::to_be_bytes(*index)[0..4]);
                }
            }
        }
    }

    Ok(stream_data)
}

fn write_incremental_trailer<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &Pdf<R>,
    root_ref: &ObjectRef,
    object_count: usize,
    previous_xref_offset: u64,
    xref_offset: usize,
    options: &WriteOptions,
) -> Result<()> {
    let mut trailer = pdf.trailer().clone();
    strip_xref_stream_trailer_keys(&mut trailer);
    trailer.insert("Size", Object::Integer(object_count as i64));
    trailer.insert("Root", Object::Reference(*root_ref));
    trailer.insert(
        "Prev",
        Object::Integer(previous_xref_offset.try_into().map_err(|_| {
            crate::Error::Unsupported("startxref offset does not fit i64".to_string())
        })?),
    );
    if options.static_id {
        apply_static_id(&mut trailer);
    } else {
        apply_random_id(&mut trailer);
    }

    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(())
}

/// Replace `trailer`'s `/ID` so the changing identifier (element 2) is qpdf's
/// static-id constant. The permanent identifier (element 1) is taken from the
/// existing `/ID` when its shape matches a 2-element array of strings; in any
/// other case (missing, wrong arity, wrong types) both elements fall back to
/// the constant — matching qpdf's behaviour on inputs without a usable `/ID`.
pub(crate) fn apply_static_id(trailer: &mut Dictionary) {
    let pi_id = Object::String(QPDF_STATIC_ID.to_vec());
    let first_id = match trailer.get("ID") {
        Some(Object::Array(values))
            if values.len() == 2
                && matches!(values[0], Object::String(_))
                && matches!(values[1], Object::String(_)) =>
        {
            values[0].clone()
        }
        _ => pi_id.clone(),
    };
    trailer.insert("ID", Object::Array(vec![first_id, pi_id]));
}

/// Ensure the trailer carries an `/ID` key for qpdf's `--deterministic-id`
/// scheme, with a dummy array of two 16-byte zero strings as its value.
///
/// Only the *presence* of the `/ID` key matters here: it makes the flat trailer
/// serializers emit `/ID` (at the position the serializer assigns it) so the
/// deterministic identifier can be written there. The stored zero value is a
/// dummy and is never serialized on the deterministic paths — those serializers
/// direct-write the real two-level identifier through their `id_writer` hook
/// (see [`write_deterministic_id_inline`]), computing it from the bytes written
/// so far. The real `/ID` cannot be a stored value because qpdf's changing
/// identifier `/ID[1]` is `md5(seed)` over a digest of the file written so far
/// (ISO 32000-2 §7.5.2 leaves the algorithm to the producer; this mirrors
/// `QPDFWriter::generateID`).
fn apply_deterministic_id_placeholder(trailer: &mut Dictionary) {
    trailer.insert(
        "ID",
        Object::Array(vec![
            Object::String(vec![0u8; 16]),
            Object::String(vec![0u8; 16]),
        ]),
    );
}

/// Generate a fresh 16-byte file identifier.
///
/// Mirrors qpdf's default-`/ID` algorithm in spirit: an MD5 digest seeded from
/// volatile per-invocation entropy (wall-clock nanoseconds, the process id, and
/// a strictly-monotonic process-global counter).  MD5 is already a direct
/// dependency, so no new crate is introduced.  The counter guarantees two calls
/// within the same nanosecond tick still produce distinct identifiers, which is
/// what makes "every save emits a different `/ID`" hold even for back-to-back
/// writes in a tight loop.
fn fresh_id_bytes() -> [u8; 16] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    let mut hasher = md5::Md5::new();
    use md5::Digest as _;
    hasher.update(nanos.to_le_bytes());
    hasher.update(pid.to_le_bytes());
    hasher.update(seq.to_le_bytes());
    hasher.finalize().into()
}

/// Replace `trailer`'s `/ID` following the default (no-flag) identifier
/// strategy of ISO 32000-1 §14.4.
///
/// The permanent identifier (element 1) is preserved from the existing `/ID`
/// when its shape matches a 2-element array of strings — i.e. on a re-save of a
/// file that already carries a well-formed `/ID`.  In every other case
/// (missing, wrong arity, wrong element types — i.e. the first save by flpdf)
/// element 1 is freshly generated.  The changing identifier (element 2) is
/// **always** freshly generated, so the `/ID` varies on every save while the
/// permanent identifier remains stable across re-saves.  This matches qpdf's
/// default observable behaviour (random `/ID`s differ between runs).
pub(crate) fn random_id_array(source_id: Option<&Object>) -> Object {
    let first_id = match source_id {
        Some(Object::Array(values))
            if values.len() == 2
                && matches!(values[0], Object::String(_))
                && matches!(values[1], Object::String(_)) =>
        {
            values[0].clone()
        }
        _ => Object::String(fresh_id_bytes().to_vec()),
    };
    let second_id = Object::String(fresh_id_bytes().to_vec());
    Object::Array(vec![first_id, second_id])
}

/// Apply [`random_id_array`] to a trailer dictionary in place, reading the
/// existing `/ID` (if any) as the permanent-identifier source.
pub(crate) fn apply_random_id(trailer: &mut Dictionary) {
    let id = random_id_array(trailer.get("ID"));
    trailer.insert("ID", id);
}

fn strip_incremental_trailer_keys(trailer: &mut Dictionary) {
    strip_xref_stream_trailer_keys(trailer);
    trailer.remove("Prev");
}

/// Remap the trailer's surviving indirect references to their Catalog-first
/// (new) numbers.
///
/// `/Root` is overwritten by the caller with the new root ref and `/Encrypt`
/// is written by [`apply_encrypt_trailer_entries`], so both are left untouched
/// here. Every other indirect value (notably `/Info`, which the renumber walk
/// always seeds) is rewritten through `map`; a value absent from the map is an
/// error rather than a stale number leaking into the output.
fn remap_trailer_refs<M: crate::rewrite_renumber::NewNumberLookup>(
    trailer: &mut Dictionary,
    map: &M,
    deleted: &[ObjectRef],
) -> Result<()> {
    // Collect the (key, old_ref) pairs first; the trailer holds only a handful
    // of entries, so the small Vec is cheaper than threading a mutable iterator.
    let to_remap: Vec<(Vec<u8>, ObjectRef)> = trailer
        .iter()
        .filter(|(key, _)| *key != b"Root" && *key != b"Encrypt")
        .filter_map(|(key, value)| match value {
            Object::Reference(r) => Some((key.to_vec(), *r)),
            _ => None,
        })
        .collect();
    for (key, old) in to_remap {
        // A trailer reference to a deleted object (e.g. `/Info` pointing at a
        // freed entry in malformed/edited input) has no body in the output and
        // is not in the renumber map. Remapping it would leave the trailer
        // pointing at a free xref row, corrupting the file on reopen — so drop
        // the key entirely instead (the object is gone; the reference is moot).
        if deleted.contains(&old) {
            trailer.remove(&key);
            continue;
        }
        let new = map.new_for_original(old).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "renumber: trailer /{} reference {old} absent from map",
                String::from_utf8_lossy(&key)
            ))
        })?;
        trailer.insert(key, Object::Reference(new));
    }
    Ok(())
}

fn strip_xref_stream_trailer_keys(trailer: &mut Dictionary) {
    let has_xref_stream_markers = matches!(trailer.get("Type"), Some(Object::Name(type_name)) if type_name.as_slice() == b"XRef")
        || trailer.get("XRefStm").is_some()
        || trailer.get("W").is_some()
        || trailer.get("Index").is_some();

    if !has_xref_stream_markers {
        return;
    }

    for key in [
        "Type",
        "F",
        "FFilter",
        "FDecodeParms",
        "W",
        "Index",
        "Length",
        "Filter",
        "DecodeParms",
        "XRefStm",
    ] {
        trailer.remove(key);
    }
}

/// Write `pdf` in canonical QDF form (qpdf's `--qdf`).
///
/// This is a thin wrapper over the canonical QDF entrypoint
/// [`write_pdf_with_options`] with `WriteOptions { qdf: true, full_rewrite:
/// true, .. }` — the same path the `flpdf qdf` / `flpdf rewrite --qdf` CLI
/// uses. It therefore goes through the QDF serializers (decoded streams,
/// indirect `/Length` holders, `%QDF-1.0` header, `%% Original object ID:`
/// comments, classic `xref` table, and the `trailer <<` dict layout).
///
/// # Errors
///
/// - [`crate::Error::Missing`] if the input has no `/Root`.
/// - [`crate::Error::Signed`] when rewriting would invalidate existing
///   signatures (this entry point always uses the full-rewrite path and does
///   not set `allow_signed_full_rewrite`).
/// - Propagates I/O errors and structural PDF errors from the underlying
///   full-rewrite writer.
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{write_qdf, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let mut out = File::create("output.qdf.pdf")?;
/// write_qdf(&mut pdf, &mut out)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn write_qdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, out: W) -> Result<()> {
    let options = WriteOptions {
        qdf: true,
        full_rewrite: true,
        ..WriteOptions::default()
    };
    write_pdf_with_options(pdf, out, &options)
}

// ---------------------------------------------------------------------------
// Full-rewrite path: decode+re-encode every stream, replaces incremental copy
// ---------------------------------------------------------------------------

/// Write `pdf` as a full non-incremental rewrite.
///
/// Every stream is decoded through its filter chain and re-encoded with a
/// single `/FlateDecode` filter.  The output has no `/Prev` chain and no
/// `ObjStm` container objects.  ObjStm member objects are emitted as ordinary
/// indirect objects.  XRef stream container objects are replaced by a freshly
/// rebuilt xref table (or xref stream, matching the input's form).
///
/// # Metadata preservation policy
///
/// The `/Info` dictionary (containing `/Producer`, `/CreationDate`, `/ModDate`,
/// `/Author`, `/Title`, `/Creator`, `/Keywords`, `/Subject`, `/Trapped`, etc.)
/// is preserved **verbatim** from the source document.  No fields are added,
/// removed, or rewritten — in particular no "modified by flpdf" suffix is
/// appended to `/Producer`.  This mirrors `qpdf`'s default behaviour
/// (`qpdf in.pdf out.pdf`) and is required for byte-identical round-trip tests.
///
/// # Scope limitations
///
/// - **ObjStm dissolve**: Object streams are dissolved — members are emitted as
///   ordinary indirect objects.  There is currently no merging of existing
///   ObjStm containers back into the regular sequence; they are simply skipped.
///   A dedicated "renumber + pack into ObjStm" pass is not yet implemented.
///
/// - **Encrypted documents**: authenticated inputs are rewritten as plaintext
///   by default; pass [`WriteOptions::encrypt`] to produce encrypted output
///   instead (currently V=4 AES-128 only).
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
mod _writer_doc_anchor {} // keeps the `write_pdf_full_rewrite` docstring above attached to its function.

fn refuse_signed_full_rewrite<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriteOptions,
) -> Result<()> {
    if options.allow_signed_full_rewrite {
        return Ok(());
    }

    let impact = signature_rewrite_impact(pdf, SignatureWriteMode::FullRewrite)?;
    if !impact.invalidates_signatures {
        return Ok(());
    }

    let mut fields: Vec<String> = match pdf.signatures() {
        Ok(signatures) => signatures
            .into_iter()
            .map(|signature| {
                if signature.field_name.is_empty() {
                    format!("{}", signature.field_ref)
                } else {
                    signature.field_name
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    fields.sort();
    fields.dedup();

    if fields.is_empty() {
        fields = impact
            .signed_object_refs
            .into_iter()
            .map(|object_ref| format!("{object_ref}"))
            .collect();
    }

    let field_list = fields.join(", ");
    let message = format!(
        "refusing full rewrite of signed PDF because it would invalidate signature field(s): \
         {field_list}. Use --remove-restrictions to explicitly allow invalidating signatures, \
         or use an incremental rewrite that preserves signed byte ranges."
    );

    Err(crate::Error::Signed { fields, message })
}

// ── Encryption context (flpdf-9hc.4.9) ───────────────────────────────────────

/// Per-write encryption state used by the full-rewrite path when
/// [`WriteOptions::encrypt`] is set. Built once at the top of
/// [`write_pdf_full_rewrite`] via [`build_encryption_context`] and consumed
/// by the per-object emission loop + the trailer-build step.
/// How the writer derives per-object string/stream encryption key material.
///
/// Mirrors the reader's per-object dispatch (`EncryptionMode`): V<5 handlers
/// derive a per-object key via Algorithm 1, while V=5 uses the 32-byte file
/// key directly with AES-256 (no per-object derivation).
#[derive(Debug, Clone, Copy)]
enum WriteCipher {
    /// V=1/V=2/V=4: per-object key via Algorithm 1, then RC4 or AES-128
    /// (the [`ObjectKeyAlg`](crate::security::standard::ObjectKeyAlg) selects
    /// the `sAlT` salt and the resulting cipher).
    PerObject(crate::security::standard::ObjectKeyAlg),
    /// V=5 R=5/R=6: the 32-byte file key is used directly with AES-256-CBC.
    /// There is no Algorithm-1 per-object derivation.
    FileKeyAes256,
}

struct EncryptionContext {
    /// Built `/Encrypt` dictionary (from a 4.1/4.2/4.3 builder).
    encrypt_dict: Dictionary,
    /// File encryption key derived from passwords + `/ID[0]` (Algorithm 2),
    /// or — for V=5 — the random 32-byte file key (FEK).
    file_key: Vec<u8>,
    /// How per-object string/stream key material is derived (V<5 per-object
    /// vs V=5 file-key-direct).
    cipher: WriteCipher,
    /// Indirect reference of the freshly-allocated `/Encrypt` object. The
    /// emission loop skips this ref so the `/Encrypt` dict itself stays
    /// plaintext (PDF 1.7 §7.6.1).
    encrypt_ref: ObjectRef,
    /// The 16-byte `/ID[0]` bytes that were fed into the file-key derivation.
    /// The output trailer's `/ID` array MUST start with these same bytes —
    /// readers re-derive the file key from `/ID[0]` to validate the password.
    id0: Vec<u8>,
    /// When `true`, all AES CBC IVs are forced to `[0u8; 16]` instead of
    /// being drawn from the OS CSPRNG.  Testing only — mirrors
    /// [`WriteOptions::static_aes_iv`].
    static_aes_iv: bool,
    /// Whether the `/Metadata` stream is encrypted alongside the rest of the
    /// document (mirrors [`crate::EncryptParams::encrypt_metadata`]). When `false`
    /// (qpdf `--cleartext-metadata`, V=4/V=5 only), the `/Metadata` stream in
    /// [`metadata_ref`](Self::metadata_ref) is left in the clear and tagged
    /// with `/Crypt /Identity` instead of being run through the cipher.
    encrypt_metadata: bool,
    /// Indirect reference of the document `/Catalog`'s `/Metadata` stream, when
    /// one exists AND `encrypt_metadata` is `false`. Used by the emission loop
    /// to exempt exactly that object from encryption. `None` whenever metadata
    /// is encrypted (the common case) or the document has no `/Metadata`.
    metadata_ref: Option<ObjectRef>,
}

/// Resolve the document `/Catalog`'s `/Metadata` indirect reference, if any.
/// Used to exempt the XMP metadata stream from encryption under
/// `--cleartext-metadata`.
fn resolve_metadata_stream_ref<R: Read + Seek>(pdf: &mut Pdf<R>) -> Option<ObjectRef> {
    let root = pdf.root_ref()?;
    match pdf.resolve_borrowed(root).ok()? {
        Object::Dictionary(dict) => dict.get_ref("Metadata"),
        _ => None,
    }
}

fn build_encryption_context<R: Read + Seek>(
    pdf: &Pdf<R>,
    options: &WriteOptions,
    params: &crate::encrypt_setup::EncryptParams,
    existing_max: u32,
    metadata_ref: Option<ObjectRef>,
) -> Result<EncryptionContext> {
    use crate::encrypt_setup::EncryptMethod;
    use crate::security::standard::{
        build_v1_v2_encrypt_dict, build_v4_encrypt_dict, ObjectKeyAlg, V1V2EncryptParams,
        V4CryptMethod, V4EncryptParams,
    };

    // Resolve /ID[0] BEFORE deriving the file key. Algorithm 2 uses /ID[0]
    // as a salt; the trailer must carry the same bytes so the reader can
    // re-derive the file key from the password.
    let id0 = resolve_id0_for_encryption(pdf, options);

    let (encrypt_dict, file_key, cipher) = match params.method {
        EncryptMethod::V4Aes128 => {
            let v4 = V4EncryptParams {
                method: V4CryptMethod::Aes,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
                encrypt_metadata: params.encrypt_metadata,
            };
            let (dict, key) = build_v4_encrypt_dict(&v4)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Aes))
        }
        EncryptMethod::V5R6Aes256 => {
            use crate::security::standard::{build_v5_r6_encrypt_dict, V5R6EncryptParams};
            // V=5 R=6 needs 68 bytes of fresh secret material (file key + four
            // 8-byte salts + 4-byte /Perms tail). Unlike V<5, /ID[0] does NOT
            // feed the key derivation — the file key is a standalone CSPRNG
            // value, so V=5 output is never byte-identical across runs.
            let secrets = generate_v5r6_secrets()?;
            let v5 = V5R6EncryptParams {
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                encrypt_metadata: params.encrypt_metadata,
            };
            let dict = build_v5_r6_encrypt_dict(&v5, &secrets);
            (dict, secrets.file_key.to_vec(), WriteCipher::FileKeyAes256)
        }
        EncryptMethod::V5R5Aes256 => {
            use crate::security::standard::{build_v5_r5_encrypt_dict, V5R6EncryptParams};
            let secrets = generate_v5r6_secrets()?;
            let v5 = V5R6EncryptParams {
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                encrypt_metadata: params.encrypt_metadata,
            };
            let dict = build_v5_r5_encrypt_dict(&v5, &secrets);
            (dict, secrets.file_key.to_vec(), WriteCipher::FileKeyAes256)
        }
        EncryptMethod::V1Rc440 => {
            // V=1 R=2 RC4-40. /EncryptMetadata is a V>=4 concept, so it is not
            // emitted here (V1V2EncryptParams has no such field).
            let v12 = V1V2EncryptParams {
                v: 1,
                r: 2,
                length_bits: 40,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
            };
            let (dict, key) = build_v1_v2_encrypt_dict(&v12)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4))
        }
        EncryptMethod::V2Rc4128 => {
            // V=2 R=3 RC4-128 (qpdf's default for `--encrypt … 128`).
            let v12 = V1V2EncryptParams {
                v: 2,
                r: 3,
                length_bits: 128,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
            };
            let (dict, key) = build_v1_v2_encrypt_dict(&v12)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4))
        }
        EncryptMethod::V4Rc4128 => {
            // V=4 R=4 with /CFM V2 (RC4-128 crypt filter), e.g. `--force-V4`.
            let v4 = V4EncryptParams {
                method: V4CryptMethod::Rc4,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
                encrypt_metadata: params.encrypt_metadata,
            };
            let (dict, key) = build_v4_encrypt_dict(&v4)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4))
        }
    };

    // `existing_max` here is the highest already-allocated number (original
    // objects plus any ObjStm container slots reserved by the caller).
    // Adding 1 gives a safe slot that cannot collide with any emitted object.
    let encrypt_num = existing_max.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported(
            "full-rewrite encrypt: /Encrypt object number overflows u32".to_string(),
        )
    })?;

    Ok(EncryptionContext {
        encrypt_dict,
        file_key,
        cipher,
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
        id0,
        static_aes_iv: options.static_aes_iv,
        encrypt_metadata: params.encrypt_metadata,
        // Only exempt the /Metadata stream when cleartext metadata was actually
        // requested (the caller passes None when encrypt_metadata is true).
        metadata_ref: if params.encrypt_metadata {
            None
        } else {
            metadata_ref
        },
    })
}

/// Generate the fresh CSPRNG secret material V=5 R=6 encryption needs: the
/// 32-byte file key, four 8-byte password salts, and the 4-byte `/Perms`
/// tail. OS-RNG failure is surfaced as [`crate::Error::Unsupported`] rather
/// than panicking (mirrors the AES-IV generation in the stream pass).
fn generate_v5r6_secrets() -> Result<crate::security::standard::V5R6Secrets> {
    let mut buf = [0u8; 68];
    getrandom::getrandom(&mut buf).map_err(|e| {
        crate::Error::Unsupported(format!(
            "OS CSPRNG (getrandom) unavailable for V=5 R=6 secret generation: {e}"
        ))
    })?;
    // Each range is a fixed, exact-length slice of `buf`, so the array
    // conversions are infallible by construction.
    Ok(crate::security::standard::V5R6Secrets {
        file_key: buf[0..32].try_into().unwrap(),
        user_validation_salt: buf[32..40].try_into().unwrap(),
        user_key_salt: buf[40..48].try_into().unwrap(),
        owner_validation_salt: buf[48..56].try_into().unwrap(),
        owner_key_salt: buf[56..64].try_into().unwrap(),
        perms_random_tail: buf[64..68].try_into().unwrap(),
    })
}

/// Build an [`EncryptionContext`] from a donor [`crate::CopyEncryptionSource`]
/// (the `--copy-encryption-from` path).
///
/// Unlike [`build_encryption_context`], this function does **not** derive a
/// file key from passwords: it takes the donor's already-recovered file key
/// verbatim, copies the donor's `/Encrypt` dictionary wholesale, and pins
/// `/ID[0]` to the donor's permanent identifier.  The output can then be
/// decrypted with the donor's original user or owner password because all the
/// ingredients of Algorithm 2 (key-length, `/O`, `/P`, `/ID[0]`) are
/// reproduced exactly.
fn build_copy_encryption_context(
    src: &crate::encrypt_setup::CopyEncryptionSource,
    options: &WriteOptions,
    existing_max: u32,
) -> Result<EncryptionContext> {
    let encrypt_num = existing_max.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported(
            "full-rewrite copy-encryption: /Encrypt object number overflows u32".to_string(),
        )
    })?;

    Ok(EncryptionContext {
        encrypt_dict: src.encrypt_dict.clone(),
        file_key: src.file_key.clone(),
        // --copy-encryption-from only supports V=4 AES-128 donors today, so the
        // donor's per-object key alg maps straight onto the per-object cipher.
        cipher: WriteCipher::PerObject(src.object_key_alg),
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
        id0: src.id0.clone(),
        static_aes_iv: options.static_aes_iv,
        // --copy-encryption-from re-encrypts every stream with the donor's
        // scheme; cleartext-metadata exemption for the copy path is out of
        // scope here (flpdf-9hc.4.9.6 covers the --encrypt path).
        encrypt_metadata: true,
        metadata_ref: None,
    })
}

/// Resolve the `/ID[0]` bytes to use for the encrypted output's file key
/// derivation AND the output trailer's `/ID` array. Preference order:
///
/// 1. Input trailer's existing `/ID[0]` (preserved across re-encrypt).
/// 2. `qpdf --static-id` constant when [`WriteOptions::static_id`] is set.
/// 3. Freshly generated random 16 bytes (via [`fresh_id_bytes`]).
fn resolve_id0_for_encryption<R: Read + Seek>(pdf: &Pdf<R>, options: &WriteOptions) -> Vec<u8> {
    if let Some(Object::Array(values)) = pdf.trailer().get("ID") {
        if values.len() == 2 {
            if let Some(Object::String(bytes)) = values.first() {
                if !bytes.is_empty() {
                    return bytes.clone();
                }
            }
        }
    }
    if options.static_id {
        QPDF_STATIC_ID.to_vec()
    } else {
        fresh_id_bytes().to_vec()
    }
}

/// Append the lowercase-hex encoding of `bytes` to `out` via a table lookup,
/// avoiding the per-byte `String` allocation a `format!("{:02x}")` loop incurs.
/// Both the fixed-width `/ID` hex form and the deterministic-ID seed must be
/// lowercase hex byte-for-byte, which this matches.
fn push_hex_lower(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
}

/// Byte length of the serialized deterministic `/ID` array value
/// `[<id0_hex><id1_hex>]`: `[` + (`<` + 32 hex + `>`) twice + `]`.
pub(crate) const DETERMINISTIC_ID_ARRAY_LEN: usize = 1 + (1 + 32 + 1) * 2 + 1;

/// Serialize a deterministic `/ID` array of two 16-byte identifiers as the
/// fixed-width hex form qpdf emits: `[<id0_hex><id1_hex>]`, exactly
/// [`DETERMINISTIC_ID_ARRAY_LEN`] bytes with no inner spaces. Building the
/// bytes by hand (rather than via [`Object::write_pdf`]) guarantees the hex
/// form even when a digest happens to be all-printable, so the value is always
/// the same fixed width regardless of its bytes. The classic linearized writer
/// calls this directly to emit the final identifier at each `/ID` site in its
/// last write pass (qpdf's 2-pass scheme); the ObjStm linearized writer uses it
/// for both the all-zero placeholder and the patched-in final value, whose equal
/// width leaves every later byte offset intact. (The flat write paths instead
/// direct-write the final value via [`write_deterministic_id_inline`].)
pub(crate) fn write_deterministic_id_array(out: &mut Vec<u8>, id0: &[u8; 16], id1: &[u8; 16]) {
    out.push(b'[');
    for id in [id0, id1] {
        out.push(b'<');
        push_hex_lower(out, id);
        out.push(b'>');
    }
    out.push(b']');
}

/// Extract the source trailer's permanent identifier `/ID[0]` when it is a
/// well-formed 2-element array whose elements are both 16-byte strings.
/// Returns `None` for any other shape (missing, wrong arity, wrong types, or a
/// length other than 16), in which case qpdf reuses the changing identifier as
/// the permanent one. The 16-byte constraint keeps the serialized `/ID` array
/// at the fixed [`DETERMINISTIC_ID_ARRAY_LEN`] the linearized writer's
/// fixed-width `/ID` emission depends on.
pub(crate) fn source_permanent_id(trailer: &Dictionary) -> Option<[u8; 16]> {
    match trailer.get("ID") {
        Some(Object::Array(values)) if values.len() == 2 => match (&values[0], &values[1]) {
            (Object::String(first), Object::String(_)) => {
                <[u8; 16]>::try_from(first.as_slice()).ok()
            }
            _ => None,
        },
        _ => None,
    }
}

/// Build the `/Info`-derived suffix of qpdf's deterministic `/ID` seed.
///
/// qpdf (`QPDFWriter::generateID`) appends, for every `/Info` entry whose value
/// is a string, `" "` followed by the string's *decoded* bytes, iterating keys
/// in sorted order (qpdf's `getKeys()` returns names sorted). Non-string
/// entries are skipped. `/Info`, and each value, may be an indirect reference,
/// so both are resolved (PDF allows any value to be indirect, ISO 32000-1
/// §7.3.10). The returned bytes are appended after `" QPDF "` to form the seed.
pub(crate) fn deterministic_id_info_suffix<R: Read + Seek>(pdf: &mut Pdf<R>) -> Vec<u8> {
    let info_obj = match pdf.trailer().get("Info").cloned() {
        Some(info) => info,
        None => return Vec::new(),
    };
    let info = match info_obj {
        Object::Reference(reference) => match pdf.resolve(reference) {
            Ok(resolved) => resolved,
            // resolve yields Ok(Null) for unknown refs and only errors on
            // I/O/parse failure, unreachable for an in-memory document.
            Err(_) => return Vec::new(), // cov:ignore: defensive resolve-error fallback
        },
        other => other,
    };
    let Object::Dictionary(dict) = info else {
        return Vec::new();
    };
    // `Dictionary::iter` already yields names in lexicographic (sorted) order,
    // matching qpdf's `getKeys()`.
    let mut suffix = Vec::new();
    for (_key, value) in dict.iter() {
        let resolved = match value {
            Object::Reference(reference) => match pdf.resolve(*reference) {
                Ok(resolved) => resolved,
                // resolve yields Ok(Null) for unknown refs and only errors on
                // I/O/parse failure, unreachable for an in-memory document.
                Err(_) => continue, // cov:ignore: defensive resolve-error fallback
            },
            other => other.clone(),
        };
        if let Object::String(bytes) = resolved {
            suffix.push(b' ');
            suffix.extend_from_slice(&bytes);
        }
    }
    suffix
}

/// Compute qpdf's two-level deterministic `/ID` from the serialized output.
///
/// `bytes` is the output written up to and including the `/ID` array's opening
/// `[`; `id_array_offset` is the inclusive end of the content digest range.
/// Mirrors `QPDFWriter::computeDeterministicIDData` + `generateID`:
///
/// 1. `det_data` = lowercase hex of `md5(bytes[0..=id_array_offset])`. The flat
///    writers call this from [`write_deterministic_id_inline`] with the offset
///    of the just-written `[`, so the range is inclusive of the `[` (qpdf
///    captures the running digest immediately after writing `" /ID ["`). The
///    linearized writer instead passes `bytes.len() - 1` to digest the whole
///    output, because a linearized file repeats `/ID` in several
///    trailers/xref-stream dicts and so has no single `[` cutoff; its all-zero
///    placeholder makes that whole-buffer digest depend only on the input,
///    keeping it self-stable across runs. qpdf computes this body digest with
///    `Pl_MD5`, which hashes the full byte range regardless of any embedded NUL
///    (unlike the seed in step 3).
/// 2. `seed` = `det_data` + `" QPDF "` + `info_suffix`.
/// 3. `/ID[1]` (changing identifier) = `md5(seed)`, but the seed is truncated at
///    its first NUL byte before hashing. qpdf hashes the seed with
///    `MD5::encodeString(seed.c_str())`, which treats the seed as a C string and
///    stops at the first NUL (`strlen`). The hex `det_data` and `" QPDF "` are
///    NUL-free, so any NUL originates in `info_suffix` (e.g. a UTF-16BE `/Info`
///    string, whose `00xx` code units carry NUL bytes); everything from the
///    first NUL onward is excluded from the changing identifier exactly as qpdf
///    excludes it.
/// 4. `/ID[0]` (permanent identifier) = `source_id0` when present, else `/ID[1]`.
pub(crate) fn compute_deterministic_id(
    bytes: &[u8],
    id_array_offset: usize,
    info_suffix: &[u8],
    source_id0: Option<[u8; 16]>,
) -> ([u8; 16], [u8; 16]) {
    use md5::Digest as _;
    let det_data = md5::Md5::digest(&bytes[..=id_array_offset]);
    // 32 hex chars for the 16-byte digest + " QPDF " (6) + the /Info suffix.
    let mut seed = Vec::with_capacity(32 + 6 + info_suffix.len());
    push_hex_lower(&mut seed, det_data.as_slice());
    seed.extend_from_slice(b" QPDF ");
    seed.extend_from_slice(info_suffix);
    // qpdf hashes the seed as a C string (`encodeString(seed.c_str())`), so it
    // stops at the first NUL. Mirror that strlen truncation; the leading hex
    // det_data and " QPDF " are NUL-free, so a NUL can only come from /Info.
    let seed_hash_input = &seed[..seed.iter().position(|&b| b == 0).unwrap_or(seed.len())];
    let id1: [u8; 16] = md5::Md5::digest(seed_hash_input).into();
    let id0 = source_id0.unwrap_or(id1);
    (id0, id1)
}

/// Direct-write qpdf's deterministic `/ID` array value INLINE at the current
/// output position, computing it from the bytes written so far.
///
/// Mirrors `QPDFWriter::generateID`: push `[`, MD5-digest the bytes written so
/// far (inclusive of the `[`, the range [`compute_deterministic_id`] expects),
/// compute the two-level identifier, then write `<id0_hex><id1_hex>]`. This
/// replaces the placeholder-then-byte-search scheme on the flat write paths, so
/// a crafted placeholder-shaped byte run elsewhere can never be mistaken for the
/// real `/ID`. The emitted bytes are identical to
/// [`write_deterministic_id_array`] for the same computed id.
pub(crate) fn write_deterministic_id_inline(
    out: &mut Vec<u8>,
    info_suffix: &[u8],
    source_id0: Option<[u8; 16]>,
) {
    out.push(b'[');
    let id_array_offset = out.len() - 1; // index of the just-pushed `[`
    let (id0, id1) = compute_deterministic_id(out, id_array_offset, info_suffix, source_id0);
    for id in [&id0, &id1] {
        out.push(b'<');
        push_hex_lower(out, id);
        out.push(b'>');
    }
    out.push(b']');
}

/// Fill the trailer's `/Encrypt` and `/ID` entries appropriately for both
/// the plaintext and encrypted output paths.
fn apply_encrypt_trailer_entries<R: Read + Seek>(
    trailer: &mut Dictionary,
    pdf: &Pdf<R>,
    options: &WriteOptions,
    encrypt_ctx: Option<&EncryptionContext>,
    deterministic_id: bool,
) {
    if let Some(ctx) = encrypt_ctx {
        // Reference the freshly-emitted /Encrypt object and pin /ID[0] to
        // the bytes the file key was derived from. /ID[1] is the changing
        // identifier (qpdf --static-id constant when --static-id is set,
        // otherwise random per save).
        trailer.insert("Encrypt", Object::Reference(ctx.encrypt_ref));
        let id1 = if options.static_id {
            QPDF_STATIC_ID.to_vec()
        } else {
            fresh_id_bytes().to_vec()
        };
        trailer.insert(
            "ID",
            Object::Array(vec![Object::String(ctx.id0.clone()), Object::String(id1)]),
        );
    } else {
        if pdf.is_encrypted() {
            trailer.remove("Encrypt");
        }
        // deterministic_id is mutually exclusive with static_id and rejected for
        // encrypted output (both guarded earlier in write_pdf_full_rewrite), so
        // it only reaches this non-encrypted arm and takes precedence. The
        // placeholder is overwritten in place after the output bytes (and thus
        // the content digest) are known; see compute_deterministic_id.
        if deterministic_id {
            apply_deterministic_id_placeholder(trailer);
        } else if options.static_id {
            apply_static_id(trailer);
        } else {
            apply_random_id(trailer);
        }
    }
}

/// Encrypt every `Object::String` value in `object`'s graph in place, using
/// a per-object key derived via Algorithm 1 and the cipher implied by
/// `ctx.object_key_alg`. The `/Encrypt` dict itself is skipped by the
/// caller (this function is not called on `ctx.encrypt_ref`).
fn encrypt_strings_in_object_for_writer(
    object_ref: ObjectRef,
    object: &mut Object,
    ctx: &EncryptionContext,
) -> Result<()> {
    use crate::security::standard::{
        encrypt_strings_in_object, per_object_key, ObjectKeyAlg, StringEncryptCipher,
    };

    let mut iv_gen = || {
        if ctx.static_aes_iv {
            [0u8; 16]
        } else {
            let mut iv = [0u8; 16];
            getrandom::getrandom(&mut iv)
                .expect("OS CSPRNG (getrandom) must be available for AES IV generation");
            iv
        }
    };

    match ctx.cipher {
        WriteCipher::PerObject(alg) => {
            let per_obj_key = per_object_key(
                &ctx.file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                alg,
            );
            match alg {
                ObjectKeyAlg::Aes => {
                    // V=4 AES per-object key is 16 bytes (min(n + 5, 16) with n=16 → 16).
                    let key_bytes: [u8; 16] = per_obj_key
                        .as_slice()
                        .try_into()
                        .expect("V=4 AES per-object key is exactly 16 bytes");
                    let cipher = StringEncryptCipher::Aes128 { key: &key_bytes };
                    encrypt_strings_in_object(
                        object_ref,
                        object,
                        cipher,
                        Some(ctx.encrypt_ref),
                        &mut iv_gen,
                    )
                }
                ObjectKeyAlg::Rc4 => {
                    let cipher = StringEncryptCipher::Rc4 {
                        key: per_obj_key.as_slice(),
                    };
                    encrypt_strings_in_object(
                        object_ref,
                        object,
                        cipher,
                        Some(ctx.encrypt_ref),
                        &mut iv_gen,
                    )
                }
            }
        }
        WriteCipher::FileKeyAes256 => {
            // V=5: the 32-byte file key is the object key directly (no
            // Algorithm-1 derivation), used with AES-256-CBC.
            let key_bytes: [u8; 32] = ctx.file_key.as_slice().try_into().map_err(|_| {
                crate::Error::Unsupported("V=5 AES-256 file key is not 32 bytes".to_string())
            })?;
            let cipher = StringEncryptCipher::Aes256 { key: &key_bytes };
            encrypt_strings_in_object(
                object_ref,
                object,
                cipher,
                Some(ctx.encrypt_ref),
                &mut iv_gen,
            )
        }
    }
}

/// Encrypt a stream's payload bytes in place (after filter re-encoding) and
/// update its `/Length` entry. AES grows the buffer by 16 bytes (IV prefix)
/// plus up to one full block of PKCS#7 padding.
fn encrypt_stream_payload_for_writer(
    object_ref: ObjectRef,
    stream: &mut crate::Stream,
    ctx: &EncryptionContext,
) -> Result<()> {
    use crate::security::standard::{
        encrypt_cipher_bytes, per_object_key, ObjectKeyAlg, StringEncryptCipher,
    };

    // AES (V=4 AESV2 or V=5 AESV3) prefixes a random 16-byte CBC IV; RC4 does
    // not. Propagate OS-RNG failures (e.g. restricted WASM sandbox, exhausted
    // entropy in a chroot at boot) as `Unsupported` instead of panicking.
    let needs_aes_iv = matches!(
        ctx.cipher,
        WriteCipher::PerObject(ObjectKeyAlg::Aes) | WriteCipher::FileKeyAes256
    );
    let mut iv = [0u8; 16];
    if needs_aes_iv && !ctx.static_aes_iv {
        getrandom::getrandom(&mut iv).map_err(|e| {
            crate::Error::Unsupported(format!(
                "OS CSPRNG (getrandom) unavailable for AES IV generation: {e}"
            ))
        })?;
    }

    match ctx.cipher {
        WriteCipher::PerObject(alg) => {
            let per_obj_key = per_object_key(
                &ctx.file_key,
                object_ref.number,
                u32::from(object_ref.generation),
                alg,
            );
            match alg {
                ObjectKeyAlg::Aes => {
                    let key_bytes: [u8; 16] = per_obj_key
                        .as_slice()
                        .try_into()
                        .expect("V=4 AES per-object key is exactly 16 bytes");
                    let cipher = StringEncryptCipher::Aes128 { key: &key_bytes };
                    encrypt_cipher_bytes(&mut stream.data, cipher, &iv)?;
                }
                ObjectKeyAlg::Rc4 => {
                    let cipher = StringEncryptCipher::Rc4 {
                        key: per_obj_key.as_slice(),
                    };
                    encrypt_cipher_bytes(&mut stream.data, cipher, &iv)?;
                }
            }
        }
        WriteCipher::FileKeyAes256 => {
            // V=5: the 32-byte file key is used directly with AES-256-CBC.
            let key_bytes: [u8; 32] = ctx.file_key.as_slice().try_into().map_err(|_| {
                crate::Error::Unsupported("V=5 AES-256 file key is not 32 bytes".to_string())
            })?;
            let cipher = StringEncryptCipher::Aes256 { key: &key_bytes };
            encrypt_cipher_bytes(&mut stream.data, cipher, &iv)?;
        }
    }

    // /Length reflects the encrypted on-disk byte count.
    let new_len = i64::try_from(stream.data.len()).map_err(|_| {
        crate::Error::Unsupported("encrypted stream /Length overflows i64".to_string())
    })?;
    stream.dict.insert("Length", Object::Integer(new_len));
    Ok(())
}

/// Re-encode a resolved stream object per the effective compression policy,
/// returning the re-encoded object and whether the **source** filter chain was
/// already a lone `/FlateDecode`.
///
/// This is the byte-critical choke point shared by `write_pdf_full_rewrite` and
/// the non-linearized generate emit path (`write_pdf_generate`). Keeping it in
/// one place prevents the two paths from drifting on qpdf's re-filter rules:
///
/// * `CompressStreams::Yes` on an already-lone-`/FlateDecode` source (and no
///   `/F` external-data entry, no `--recompress-flate`) is **preserved verbatim**
///   — qpdf does not decode + re-encode it — with `/Length` normalized to the raw
///   data length.
/// * any other `Yes`/`No` policy decodes and re-encodes via
///   [`apply_stream_compress_policy`].
/// * preserve mode (`None`) passes the dict + raw bytes through unchanged.
///
/// The returned bool feeds [`write_reencoded_object`], which only appends a
/// regenerated `/Filter` (qpdf's re-filtered key order) when the source was NOT
/// already a lone `/FlateDecode`.
fn reencode_stream_for_compress(stream: crate::Stream, options: &WriteOptions) -> (Object, bool) {
    let source_filter_is_lone_flate = is_lone_flate(stream.dict.get("Filter"));
    let reencoded = match effective_stream_policy(options) {
        // qpdf preserves an already-lone-/FlateDecode stream verbatim under the
        // compress policy (no decode + re-encode) unless recompression is
        // explicitly requested. Normalize /Length to the raw data length (a
        // source may carry an indirect /Length).
        //
        // Exclude external streams: a `/F` entry means the canonical data lives
        // in an external file and the in-body bytes are not authoritative, so
        // preserving them verbatim would keep a stale external reference. Such
        // streams fall through to the re-encode arm, which embeds the decoded
        // data and strips `/F` / `/FFilter` / `/FDecodeParms`.
        Some(CompressStreams::Yes)
            if source_filter_is_lone_flate
                && !options.recompress_flate
                && stream.dict.get("F").is_none() =>
        {
            let mut stream = stream;
            let len = i64::try_from(stream.data.len()).unwrap_or(i64::MAX);
            stream.dict.insert("Length", Object::Integer(len));
            Object::Stream(stream)
        }
        Some(compress_policy) => apply_stream_compress_policy(&stream, compress_policy),
        // Preserve mode: pass dict + raw bytes verbatim, no decode/re-encode.
        None => Object::Stream(stream),
    };
    (reencoded, source_filter_is_lone_flate)
}

/// Append a re-encoded object's body to `bytes` in qpdf's **non-qdf**
/// serialization order. For a stream, a regenerated lone `/Filter /FlateDecode`
/// is emitted last (and `/Length` first) only when the compress policy
/// re-encoded a source that was NOT already a lone `/FlateDecode`
/// (`write_stream_to_buf_qpdf_order`); an already-Flate or preserved source keeps
/// its lexicographic order with `/Length` last. Non-stream objects serialize
/// normally. Shared by `write_pdf_full_rewrite` and `write_pdf_generate`.
fn write_reencoded_object(
    bytes: &mut Vec<u8>,
    reencoded: &Object,
    source_filter_is_lone_flate: bool,
    options: &WriteOptions,
) {
    if let Object::Stream(s) = reencoded {
        let refiltered = matches!(effective_stream_policy(options), Some(CompressStreams::Yes))
            && !source_filter_is_lone_flate
            && is_lone_flate(s.dict.get("Filter"));
        write_stream_to_buf_qpdf_order(bytes, s, options.newline_before_endstream, refiltered);
    } else {
        // cov:ignore-start: unreachable — callers only pass stream objects and
        // reencode_stream_for_compress always returns Object::Stream.
        reencoded.write_pdf(bytes);
        // cov:ignore-end
    }
}

fn write_pdf_full_rewrite<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> Result<()> {
    // ── ObjStm-routing overview ───────────────────────────────────────────────
    // 1. Run the planner to decide which objects belong in ObjStm batches.
    // 2. Build a member→(container_num, index_in_batch) lookup.
    // 3. Allocate container object numbers above the highest input number.
    // 4. Emission loop: skip members (they'll be emitted via containers).
    // 5. After the loop: emit each container as a stream object.
    // 6. xref: for Stream form, insert Compressed entries for members.
    //    For Table form with non-empty batches: return Err (5.7 guard).
    // ─────────────────────────────────────────────────────────────────────────

    // Non-linearized --object-streams=generate is byte-identical to qpdf only
    // with qpdf's generate-mode numbering (container numbered immediately before
    // its members, members ascending-source, even split into ceil(n/100)
    // containers). That layout differs structurally from this path's
    // Catalog-first + containers-above-max scheme, so route it to the dedicated
    // emitter. Restricted to the plain case: --qdf forces ObjStm off (it always
    // emits a classic xref table), and --encrypt / --copy-encryption-from keep
    // the containers-above-max scheme here (their /Encrypt slot allocation
    // depends on it).
    if matches!(options.object_streams, ObjectStreamMode::Generate)
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && !options.qdf
    {
        return write_pdf_generate(pdf, out, options);
    }

    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    // Catalog-first renumber (flpdf-9hc.32): assign output object numbers in
    // qpdf's `enqueueObjectsStandard` BFS order so that plain rewrite output is
    // byte-identical to `qpdf --static-id`. `build` borrows `pdf` mutably (lazy
    // load) and returns an owned map, releasing the borrow before the loop.
    let renumber = crate::rewrite_renumber::CatalogFirstRenumber::build(pdf)?;
    // The new /Root reference (always seeded first by the walk, so present).
    let new_root = renumber
        .new_for_original(root_ref)
        .ok_or_else(|| crate::Error::Unsupported("renumber: /Root absent from map".to_string()))?;

    refuse_signed_full_rewrite(pdf, options)?;

    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }

    // Pass `false` here because full-rewrite ObjStm emission is only known
    // after planning. The required PDF 1.5 floor is applied below from the
    // final xref form, which becomes `Stream` when ObjStm batches are emitted.
    let mut version = effective_pdf_version(pdf.version(), options, false, false).to_owned();

    // ── encryption preflight (flpdf-9hc.4.9 / 4.11 / 4.16 / 4.17) ─────────
    // --encrypt supports xref-stream form and ObjStm containers (flpdf-9hc.4.16
    // / 4.17).  --copy-encryption-from still forces classic xref Table (ObjStm
    // on the copy path is not yet tested).  Reject incompatible flag
    // combinations upfront with a clear diagnostic.
    //
    // Invariant: at most ONE of encrypt / copy_encryption is set.  The CLI
    // enforces this via conflicts_with; guard here too so a library caller
    // that passes both gets a recoverable error rather than a panic.
    if options.encrypt.is_some() && options.copy_encryption.is_some() {
        return Err(crate::Error::Unsupported(
            "encrypt and copy_encryption are mutually exclusive".to_string(),
        ));
    }
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();

    if options.deterministic_id && encrypting {
        // qpdf rejects this combination: the /ID feeds the encryption key, so a
        // content-derived /ID cannot be computed before the encrypted bytes
        // exist. Mirror qpdf's user-facing wording.
        return Err(crate::Error::Unsupported(
            "the deterministic-id option is incompatible with encrypted output files".to_string(),
        ));
    }

    if encrypting && options.qdf {
        return Err(crate::Error::Unsupported(
            "--encrypt / --copy-encryption-from cannot be combined with --qdf \
             (flpdf-9hc.4.9 walking skeleton)"
                .to_string(),
        ));
    }

    // Capture qpdf's deterministic-`/ID` seed inputs from the ORIGINAL trailer
    // before the emission loop borrows `pdf`: the permanent identifier `/ID[0]`
    // (preserved when well-formed) and the `/Info`-derived seed suffix. qpdf
    // reads these from the source trailer (`m->pdf.getTrailer()`), not the
    // remapped output trailer, so both are gathered here while `pdf` is free.
    let (det_id_source_id0, det_id_info_suffix): (Option<[u8; 16]>, Vec<u8>) =
        if options.deterministic_id {
            let id0 = source_permanent_id(pdf.trailer());
            let suffix = deterministic_id_info_suffix(pdf);
            (id0, suffix)
        } else {
            (None, Vec::new())
        };

    // ── Step 1: run the ObjStm planner ───────────────────────────────────────
    // For --encrypt: ObjStm containers encrypt as a single blob per PDF 1.7
    // §7.5.7; the container stream is encrypted via encrypt_stream_payload_for_writer
    // (Step 5 below). Per-member string encryption is skipped because members
    // are not emitted in the main loop.
    // For --copy-encryption-from: keep ObjStm off (the copy path doesn't yet
    // allocate container numbers above the /Encrypt slot).
    let planner_options;
    let planner_config = if options.copy_encryption.is_some() {
        planner_options = WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            ..options.clone()
        };
        object_streams::planner_config_from_options(&planner_options)
    } else {
        object_streams::planner_config_from_options(options)
    };
    let mut plan = object_streams::plan_object_streams(pdf, &planner_config)?;

    // Drop ObjStm members that are not reachable from the trailer seed. The
    // planner draws candidates from the full live-object universe with a
    // type-only eligibility filter, so an eligible-but-unreachable object
    // (e.g. an orphan dict referenced by nothing) can be batched even though
    // the Catalog-first renumber map (which drives emission) omits it. Such an
    // object has no NEW number, so leaving it in a batch would make the
    // renumber-map lookups below fail and abort the whole write. Filtering
    // here — before the `plan.batches.is_empty()` xref-form decision below —
    // drops the orphan from every container; the main emit loop already only
    // emits objects present in the renumber map, so the orphan disappears
    // cleanly (qpdf-consistent, matching flpdf's qdf/disable paths).
    for batch in &mut plan.batches {
        batch.retain(|member| renumber.new_for_original(*member).is_some());
    }
    plan.batches.retain(|batch| !batch.is_empty());

    // Xref form selection: ObjStm-resident objects need type-2 xref entries,
    // which can only live in xref streams.  When the planner emits any batch
    // we therefore force-upgrade to `Stream` even if the source used a
    // classic xref table.  An empty plan respects the source form, so a
    // Disable-mode rewrite of a Table-form input still produces a classic
    // xref table.
    let mut effective_xref_form = if plan.batches.is_empty() {
        pdf.last_xref_form()
    } else {
        XrefForm::Stream
    };

    // QDF mode always uses the classic xref table for human readability —
    // override whatever the planner or source form selected.
    // user-facing diagnostic for explicit --object-streams + --qdf is emitted
    // by the CLI layer (flpdf-9hc.6.8)
    if options.qdf {
        effective_xref_form = XrefForm::Table;
    }

    // --copy-encryption-from: keep xref Table (its /Encrypt slot is at
    // existing_max+1 with no containers; xref stream support is a follow-up).
    if options.copy_encryption.is_some() {
        effective_xref_form = XrefForm::Table;
    }

    // PDF 1.5 introduced xref streams.  Bump the header floor to 1.5 whenever
    // the chosen xref form is `Stream`, overriding even an explicit
    // `--force-version` lower than 1.5.  This applies both when the source
    // was already a stream and when we just upgraded for ObjStm output.
    if matches!(effective_xref_form, XrefForm::Stream)
        && parse_pdf_version(&version).is_none_or(|v| v < (1, 5))
    {
        version = "1.5".to_string();
    }

    // /V-based PDF header floor.  This fires independently of xref form: even
    // when the xref-stream bump above (lines 2102-2106) has already raised the
    // header to 1.5, a V=5/R=6 output still needs this floor to push from 1.5
    // to 1.7.  For a classic-table source with no ObjStm batches the bump does
    // not fire at all, making this floor the only mechanism that prevents e.g.
    // a 1.4 input encrypted as V=4 from emitting a spec-violating 1.4 header.
    // /V 1 (R=2) ⇒ 1.1, /V 2 ⇒ 1.4, /V 4 ⇒ 1.5, /V 5 ⇒ 1.7.
    if let Some(params) = options.encrypt.as_ref() {
        use crate::encrypt_setup::EncryptMethod;
        let floor = match params.method {
            EncryptMethod::V1Rc440 => (1, 1),
            EncryptMethod::V2Rc4128 => (1, 4),
            EncryptMethod::V4Aes128 | EncryptMethod::V4Rc4128 => (1, 5),
            EncryptMethod::V5R6Aes256 | EncryptMethod::V5R5Aes256 => (1, 7),
        };
        if parse_pdf_version(&version).is_none_or(|v| v < floor) {
            version = format!("{}.{}", floor.0, floor.1);
        }
    } else if options.copy_encryption.is_some() {
        // --copy-encryption-from only supports V=4 AES-128 donors today, which
        // carry /V 4 and therefore require the same >= 1.5 header floor as the
        // --encrypt V=4 path. (encrypt / copy_encryption are mutually exclusive.)
        if parse_pdf_version(&version).is_none_or(|v| v < (1, 5)) {
            version = "1.5".to_string();
        }
    }

    // ── Step 2 & 3: build member→batch lookup and allocate container numbers ─
    // Drive emission from the Catalog-first map: `(new_ref, old_ref)` pairs in
    // ascending new-number order. The new numbers are a contiguous `1..=N`, so
    // `existing_max` is simply `N` and aux objects (ObjStm containers,
    // /Encrypt, qdf length-holders) allocate above it. Object 0 / deleted refs
    // are never reachable from /Root, so they never appear here.
    let renumbered: Vec<(ObjectRef, ObjectRef)> = renumber.pairs().collect();

    let existing_max: u32 = u32::try_from(renumber.len()).map_err(|_| {
        crate::Error::Unsupported("full-rewrite: renumbered object count overflows u32".to_string())
    })?;

    // Allocate a fresh object number for each container above existing_max.
    let container_refs: Vec<ObjectRef> = (1..=plan.batches.len())
        .map(|i| {
            existing_max
                .checked_add(i as u32)
                .map(|n| ObjectRef::new(n, 0))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            crate::Error::Unsupported(
                "full-rewrite: ObjStm container number overflows u32".to_string(),
            )
        })?;

    // member_to_batch: ORIGINAL ObjectRef → (container_obj_num, index_in_batch).
    // Keyed on ORIGINAL refs because the main emit loop tests membership against
    // each object's ORIGINAL ref to decide whether to skip it (it lives in an
    // ObjStm instead of being emitted as a plain indirect).
    use std::collections::HashMap;
    let mut member_to_batch: HashMap<ObjectRef, (u32, u32)> = HashMap::new();
    // member_new_to_batch: NEW member object number → (container_obj_num,
    // index_in_batch). Keyed on NEW numbers because the type-2 (compressed)
    // xref entries are written in NEW-number space; each member's xref slot must
    // be located by the number it carries in the renumbered output.
    let mut member_new_to_batch: HashMap<u32, (u32, u32)> = HashMap::new();
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_num = container_refs[batch_idx].number;
        for (idx_in_batch, &member_ref) in batch.iter().enumerate() {
            member_to_batch.insert(member_ref, (container_num, idx_in_batch as u32));
            // ObjStm members are reachable objects (Catalog/Pages/etc.), so they
            // are present in the Catalog-first renumber map. A member absent from
            // the map is a planner/renumber inconsistency — surface it.
            let new = renumber.new_for_original(member_ref).ok_or_else(|| {
                crate::Error::Unsupported("ObjStm member absent from renumber map".to_string())
            })?;
            member_new_to_batch.insert(new.number, (container_num, idx_in_batch as u32));
        }
    }

    // ── flpdf-9hc.4.9 / 4.11 / 4.16: encryption context ────────────────────
    // Built ONCE up front so /ID[0] is decided before any object is encrypted.
    // For --encrypt: /Encrypt is allocated above existing objects AND any ObjStm
    // container numbers (existing_max+1..existing_max+N), so base_for_encrypt+1
    // is the safe slot (flpdf-9hc.4.16).
    // For --copy-encryption-from: ObjStm is forced off, so existing_max+1 is safe.
    // Resolve /Metadata stream ref up front for --cleartext-metadata support.
    let metadata_ref = if options
        .encrypt
        .as_ref()
        .is_some_and(|p| !p.encrypt_metadata)
    {
        resolve_metadata_stream_ref(pdf)
    } else {
        None
    };
    let encrypt_ctx: Option<EncryptionContext> = if let Some(ref params) = options.encrypt {
        let containers_len = u32::try_from(plan.batches.len()).map_err(|_| {
            crate::Error::Unsupported(
                "full-rewrite encrypt: ObjStm batch count overflows u32".to_string(),
            )
        })?;
        let base_for_encrypt = existing_max.checked_add(containers_len).ok_or_else(|| {
            crate::Error::Unsupported(
                "full-rewrite encrypt: /Encrypt object number overflows u32".to_string(),
            )
        })?;
        Some(build_encryption_context(
            pdf,
            options,
            params,
            base_for_encrypt,
            metadata_ref,
        )?)
    } else if let Some(ref src) = options.copy_encryption {
        Some(build_copy_encryption_context(src, options, existing_max)?)
    } else {
        None
    };

    // ── QDF length-holder pre-pass (flpdf-9hc.6.12) ──────────────────────────
    // In qdf mode every real stream's /Length is emitted as an INDIRECT
    // reference `/Length H 0 R` plus a separate bare-integer holder object,
    // matching qpdf 11.9.0 --qdf and the flpdf::fix_qdf oracle (qdf_fix.rs).
    // A direct `/Length <n>` in a stream dict is something flpdf::fix_qdf
    // never rewrites, so without this the "edit then fix" use case breaks.
    //
    // Idempotence: a qdf output fed back through this path already carries
    // `/Length H 0 R` plus a leaf-integer holder object H. We detect those
    // holders here so we (a) skip re-emitting them as ordinary objects in
    // the main loop and (b) REUSE their number H rather than allocating a
    // fresh one — making `flpdf --qdf` of its own qdf output byte-stable.
    use std::collections::HashSet;
    // Holder numbers found here are recorded in NEW-number space, because the
    // /Length reference inside each stream is rewritten by
    // `renumber_refs_in_place` before it is consulted in the main loop. We
    // resolve via the ORIGINAL ref (`old_ref`) but map the holder's old number
    // through `renumber` to the new number used downstream.
    let mut existing_holders: HashSet<u32> = HashSet::new();
    if options.qdf {
        for (_new_ref, old_ref) in &renumbered {
            // `pdf.encryption_ref()` and `member_to_batch` are original-space.
            if Some(*old_ref) == pdf.encryption_ref() || member_to_batch.contains_key(old_ref) {
                continue;
            }
            if let Ok(Object::Stream(s)) = pdf.resolve_borrowed(*old_ref) {
                if let Some(Object::Reference(r)) = s.dict.get("Length") {
                    if let Some(new_holder) = renumber.new_for_original(*r) {
                        existing_holders.insert(new_holder.number);
                    }
                }
            }
        }
    }

    // Holders to emit after the last original object, ascending by number:
    // holder_object_number -> decoded_length_value. A BTreeMap keeps them
    // unique and key-sorted; a reused holder number with a *conflicting*
    // length is an explicit error (a plain Vec + dedup_by_key would silently
    // drop the conflict and emit a wrong `/Length H 0 R`).
    let mut length_holders: BTreeMap<u32, i64> = BTreeMap::new();
    // Running counter for freshly allocated holder numbers (above existing_max
    // and above any ObjStm container numbers). Mirrors the `existing_max + i`
    // container-allocation pattern at the top of this function.
    let mut next_holder_offset: u32 = plan.batches.len() as u32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);
    if options.qdf {
        bytes.extend_from_slice(b"%QDF-1.0\n");
        bytes.extend_from_slice(b"\n");
    }

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();

    // qpdf never emits a body object for object 0 (the xref free-list head) or
    // for any free/deleted entry — in any mode (plain rewrite, --qdf, or
    // object-stream output). Those entries exist only as `f` rows in the
    // regenerated xref table. flpdf's `object_refs()` includes the free head
    // (0 65535) and any deleted refs, so we suppress them here unconditionally.
    // (flpdf-9hc.6.10 first added this on the qdf path; flpdf-9hc.31 extended
    // it to the plain path — qpdf parity requires it in every mode, and without
    // it the plain rewrite leaks `0 65535 obj null` as a body object, shifting
    // every subsequent offset and blocking bytes-identical output.)
    //
    // Kept as the `Vec` `deleted_object_refs()` returns rather than collected
    // into a `HashSet`: deleted objects are typically zero or a handful, so a
    // linear `contains` over a contiguous slice beats hashing plus a heap
    // allocation for that size.
    let skip_refs = pdf.deleted_object_refs();

    for (new_ref, old_ref) in &renumbered {
        // `pdf.encryption_ref()`, `skip_refs` (deleted_object_refs), and
        // `member_to_batch` are all keyed on ORIGINAL refs, so compare with
        // `old_ref`. (Object 0 / deleted refs are unreachable from /Root and
        // never appear in `renumbered`, but the guards stay for parity.)
        if Some(*old_ref) == pdf.encryption_ref() {
            continue;
        }

        // Never emit object 0 or any free/deleted entry as a body object (qpdf
        // parity, all modes). The xref free-list head and any free rows are
        // still written into the regenerated `xref` table below.
        if old_ref.number == 0 || skip_refs.contains(old_ref) {
            continue;
        }

        // ── Step 4: skip members that will be routed into an ObjStm batch ───
        if member_to_batch.contains_key(old_ref) {
            continue;
        }

        // QDF length-holder objects from a prior qdf pass are reconstructed
        // (with the recomputed length) below; do not re-emit them here as
        // ordinary integer objects, or idempotence breaks. `existing_holders`
        // is in NEW-number space, so compare against `new_ref`.
        if options.qdf && existing_holders.contains(&new_ref.number) {
            continue;
        }

        // Resolve the object via its ORIGINAL ref; propagate the error so
        // callers see corrupt input rather than a silent success with missing
        // /Root descendants.
        let mut object = pdf.resolve(*old_ref)?;

        // Rewrite every internal reference to its new number BEFORE any
        // string-encryption or stream policy looks at the object, so all
        // downstream code sees new-number space.
        crate::rewrite_renumber::renumber_refs_in_place(&mut object, &renumber)?;

        // flpdf-9hc.4.9: encrypt every string inside this object's resolved
        // graph. Stream PAYLOAD encryption happens later (after the compress
        // policy reencode), and the /Encrypt dict object itself is exempt per
        // PDF 1.7 §7.6.1 ("strings and streams inside the encryption
        // dictionary are not encrypted").
        // `ctx.encrypt_ref` is the freshly-allocated output /Encrypt slot (above
        // the new max), so it lives in NEW-number space — compare and key the
        // per-object encryption against `new_ref`, NOT `old_ref`. The encryption
        // key derives from the object number, so it MUST be the new number.
        if let Some(ctx) = &encrypt_ctx {
            if *new_ref != ctx.encrypt_ref {
                encrypt_strings_in_object_for_writer(*new_ref, &mut object, ctx)?;
            }
        }

        // Skip xref-stream container objects — we'll rebuild the xref from
        // scratch below.  Skip ObjStm container objects — their members have
        // already been resolved into the cache as individual objects and are
        // emitted in the main loop.
        if let Object::Stream(ref s) = object {
            let ty = s.dict.get("Type");
            let is_xref = matches!(ty, Some(Object::Name(n)) if n.as_slice() == b"XRef");
            let is_objstm = matches!(ty, Some(Object::Name(n)) if n.as_slice() == b"ObjStm");
            if is_xref || is_objstm {
                continue;
            }
        }

        // Duplicate detection (same contract as write_qdf). `offsets` is keyed
        // on the emitted (NEW) number.
        if offsets.contains_key(&new_ref.number) {
            return Err(crate::Error::Unsupported(format!(
                "duplicate object number {} in xref table",
                new_ref.number
            )));
        }

        // QDF per-object comment: "%% Original object ID: N G"
        // Emitted immediately before the "N G obj" line so human readers can
        // locate objects without consulting the xref table.  Mirrors qpdf
        // 11.9.0 --qdf output.  Suppressed when no_original_object_ids=true.
        // The xref offset below is recorded AFTER the comment so it still
        // points at the "N G obj" line, not at the comment.
        // The comment records the ORIGINAL object id (qpdf prints the pre-
        // renumber number here), so use `old_ref`.
        if options.qdf && !options.no_original_object_ids {
            bytes.extend_from_slice(
                format!(
                    "%% Original object ID: {} {}\n",
                    old_ref.number, old_ref.generation
                )
                .as_bytes(),
            );
        }

        // The body header uses the emitted (NEW) number.
        let emit_offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes(),
        );

        if let Object::Stream(stream) = object {
            // Determine the effective stream policy.
            // QDF always wins (decoded), and effective_stream_policy handles
            // that; None means preserve mode: emit the stream verbatim.
            // Capture the pre-policy /Length form so an existing indirect
            // holder (from a prior qdf pass) can be reused for byte-stable
            // idempotence instead of allocating a new number.
            let prior_holder = match stream.dict.get("Length") {
                Some(Object::Reference(r)) if existing_holders.contains(&r.number) => {
                    Some(r.number)
                }
                _ => None,
            };
            // qpdf re-filters (decode + re-encode to a single /FlateDecode, and
            // emits /Length before the regenerated /Filter) only for streams
            // whose source filter chain is NOT already a lone /FlateDecode. An
            // already-Flate source is preserved by qpdf and keeps lexicographic
            // dict order with /Length last. Capture that from the source filter
            // so the output ordering tracks qpdf's re-filter decision rather
            // than flpdf's unconditional decode/re-encode.
            // Re-encode per the compress policy via the shared choke point so
            // this path and the generate emit path cannot drift on qpdf's
            // re-filter rules. `reencoded` is owned and `mut` because the
            // encryption step below may rewrite the stream payload in place.
            let (mut reencoded, source_filter_is_lone_flate) =
                reencode_stream_for_compress(stream, options);

            // flpdf-9hc.4.9: encrypt the stream payload AFTER any filter
            // re-encoding, so the encryption operates on the on-disk bytes.
            // /Length is updated to the encrypted byte count (AES-CBC adds a
            // 16-byte IV prefix and PKCS#7 padding). Skip for the /Encrypt
            // object itself.
            // `ctx.encrypt_ref` is the output /Encrypt slot (NEW space) and the
            // payload key derives from the object number, so both the skip-check
            // and the key use `new_ref`. `ctx.metadata_ref` comes from
            // `resolve_metadata_stream_ref`, an ORIGINAL ref, so it is compared
            // against `old_ref`.
            if let Some(ctx) = &encrypt_ctx {
                if *new_ref != ctx.encrypt_ref {
                    if let Object::Stream(ref mut s) = reencoded {
                        if !ctx.encrypt_metadata && ctx.metadata_ref == Some(*old_ref) {
                            // --cleartext-metadata: leave the /Metadata XMP
                            // stream in the clear and prepend /Crypt /Identity so
                            // readers know not to decrypt it (flpdf-9hc.4.9.6).
                            // /Length stays as the un-encrypted byte count.
                            crate::security::standard::prepend_crypt_filter_to_stream_dict(
                                &mut s.dict,
                                b"Identity",
                            );
                        } else {
                            encrypt_stream_payload_for_writer(*new_ref, s, ctx)?;
                        }
                    }
                }
            }

            if options.qdf {
                if let Object::Stream(ref s) = reencoded {
                    // QDF: split the stream's /Length into a separate
                    // indirect length-holder object so flpdf::fix_qdf
                    // (qdf_fix.rs) — which only ever rewrites an INDIRECT
                    // `/Length M G R`, never a direct one — can repair the
                    // length after a hand edit, then replace the dict entry
                    // with `H 0 R`.
                    //
                    // The holder body is the ON-DISK byte count (raw `data`
                    // plus the single EOL the framing inserts before
                    // `endstream`), NOT `apply_stream_compress_policy`'s raw
                    // decoded length. fix_qdf counts the emitted bytes between
                    // `stream`+EOL and the `endstream` line; the holder must
                    // equal that, or the writer and fix_qdf disagree by the
                    // newline-before-endstream byte and the "edit then fix"
                    // loop is broken (flpdf-9hc.6.12).
                    let len_value = i64::try_from(on_disk_stream_len(
                        &s.data,
                        options.newline_before_endstream,
                    ))
                    .unwrap_or(i64::MAX);
                    let holder = match prior_holder {
                        Some(h) => h,
                        None => {
                            next_holder_offset =
                                next_holder_offset.checked_add(1).ok_or_else(|| {
                                    crate::Error::Unsupported(
                                        "full-rewrite: QDF length-holder number overflows u32"
                                            .to_string(),
                                    )
                                })?;
                            existing_max
                                .checked_add(next_holder_offset)
                                .ok_or_else(|| {
                                    crate::Error::Unsupported(
                                        "full-rewrite: QDF length-holder number overflows u32"
                                            .to_string(),
                                    )
                                })?
                        }
                    };
                    if let Some(prev) = length_holders.insert(holder, len_value) {
                        if prev != len_value {
                            return Err(crate::Error::Unsupported(format!(
                                "full-rewrite: QDF length-holder {holder} reused with conflicting lengths ({prev} vs {len_value})"
                            )));
                        }
                    }

                    let mut holder_stream = s.clone();
                    holder_stream
                        .dict
                        .insert("Length", Object::Reference(ObjectRef::new(holder, 0)));
                    write_stream_to_buf_qdf(
                        &mut bytes,
                        &holder_stream,
                        options.newline_before_endstream,
                    );
                } else {
                    // cov:ignore-start: unreachable — this arm is inside the
                    // stream branch and reencode_stream_for_compress always
                    // returns Object::Stream.
                    reencoded.write_pdf_qdf(&mut bytes, 0);
                    // cov:ignore-end
                }
            } else {
                // Non-qdf: shared choke point — qpdf's re-filtered key order for
                // re-encoded streams, lexicographic order otherwise. Identical to
                // the generate emit path (`write_pdf_generate`).
                write_reencoded_object(
                    &mut bytes,
                    &reencoded,
                    source_filter_is_lone_flate,
                    options,
                );
            }
        } else if options.qdf {
            object.write_pdf_qdf(&mut bytes, 0);
        } else {
            object.write_pdf(&mut bytes);
        }

        bytes.extend_from_slice(b"\nendobj\n");
        // QDF framing (flpdf-9hc.6.10): qpdf `--qdf` separates every indirect
        // object with one blank line (`endobj\n\n%% Original object ID:` …, and
        // `endobj\n\nxref` before the xref table). The trailing blank line is
        // also emitted before the next holder/ObjStm object and, because
        // `xref_offset` is captured immediately after the loops, before the
        // `xref` keyword for the final object — matching qpdf byte-for-byte.
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(new_ref.number, (new_ref.generation, emit_offset));
    }

    // ── Step 5: emit each ObjStm container ───────────────────────────────────
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_ref = container_refs[batch_idx];
        // Resolve each member by its ORIGINAL ref, rewrite its internal
        // references to NEW numbers, and pair it with its NEW ref so the ObjStm
        // pair table records the renumbered member number. Without this the
        // members keep their old numbers and old internal links, corrupting the
        // Catalog-first output (e.g. Catalog /Pages would dangle).
        let mut resolved: Vec<(ObjectRef, Object)> = Vec::with_capacity(batch.len());
        for &old in batch {
            let mut obj = pdf.resolve(old)?;
            crate::rewrite_renumber::renumber_refs_in_place(&mut obj, &renumber)?;
            let new = renumber.new_for_original(old).ok_or_else(|| {
                crate::Error::Unsupported("ObjStm member absent from renumber map".to_string())
            })?;
            resolved.push((new, obj));
        }
        let body = object_streams::emit_objstm_body_from_resolved(&resolved)?;
        let mut stream = object_streams::wrap_objstm_body(&body, options.compress_streams)?;
        // Encrypt the ObjStm container as a single blob (PDF 1.7 §7.5.7).
        // Member objects' strings are NOT individually encrypted; the container
        // stream's encryption covers them all.
        if let Some(ctx) = &encrypt_ctx {
            encrypt_stream_payload_for_writer(container_ref, &mut stream, ctx)?;
        }

        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", container_ref.number).as_bytes());
        write_stream_to_buf(&mut bytes, &stream, options.newline_before_endstream);
        bytes.extend_from_slice(b"\nendobj\n");
        // QDF inter-object blank-line separator (flpdf-9hc.6.10). qdf mode
        // emits no ObjStm containers (6.2), so this is a consistency guard.
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(container_ref.number, (0, emit_offset));
    }

    // ── Step 5b: emit QDF length-holder objects (flpdf-9hc.6.12) ─────────────
    // Each real stream's /Length was rewritten to `/Length H 0 R` above.
    // Emit the holders here, AFTER the last original object (and any ObjStm
    // container — empty in qdf mode per 6.2), ascending by object number,
    // with NO `%% Original object ID:` comment (they are synthetic; qpdf
    // 11.9.0 --qdf only emits that comment for source objects). The holder
    // body is the decoded byte count as a bare decimal integer on its own
    // line with no zero padding — identical framing to qpdf and to what
    // flpdf::fix_qdf (qdf_fix.rs) rewrites. The same `endobj` framing the
    // main loop uses keeps offsets and xref consistent.
    // `length_holders` is a BTreeMap: already unique and ascending by holder
    // number, with conflicting-length reuse rejected at insert time above.
    for (holder, len_value) in &length_holders {
        if offsets.contains_key(holder) {
            return Err(crate::Error::Unsupported(format!(
                "full-rewrite: QDF length-holder number {holder} collides with an existing object"
            )));
        }
        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{holder} 0 obj\n{len_value}\nendobj\n").as_bytes());
        // QDF inter-object blank-line separator (flpdf-9hc.6.10): a blank line
        // between holders and before the `xref` keyword, matching qpdf which
        // separates every object. length_holders is only populated on the qdf
        // path, but gate explicitly for clarity.
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(*holder, (0, emit_offset));
    }

    // ── flpdf-9hc.4.9: emit the /Encrypt dictionary as a plaintext indirect
    // object. Per PDF 1.7 §7.6.1 the /Encrypt dict itself is never encrypted;
    // its strings (/U /O /UE /OE /Perms) are already in their final wire form
    // from the dict builders.
    if let Some(ctx) = &encrypt_ctx {
        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", ctx.encrypt_ref.number).as_bytes());
        Object::Dictionary(ctx.encrypt_dict.clone()).write_pdf(&mut bytes);
        bytes.extend_from_slice(b"\nendobj\n");
        offsets.insert(ctx.encrypt_ref.number, (0, emit_offset));
    }

    // Build xref / trailer matching the input's xref form.
    let xref_offset = bytes.len();
    // `object_count` is the smallest object number strictly greater than every
    // emitted one — i.e. the number we'll assign to a freshly created xref
    // stream object.  Using `saturating_add` here would silently fail when the
    // input's highest object number is `u32::MAX`: we'd reuse that exact
    // number for the xref stream and collide with an existing object.  Use
    // `checked_add` so the overflow surfaces as an explicit error instead.
    let max_object_number = offsets.keys().next_back().copied().unwrap_or(0);
    let object_count: usize = max_object_number
        .checked_add(1)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            crate::Error::Unsupported("full-rewrite: object count does not fit in u32".to_string())
        })?;

    match effective_xref_form {
        XrefForm::Table => {
            // Classic xref table.
            bytes.extend_from_slice(format!("xref\n0 {}\n", object_count).as_bytes());
            bytes.extend_from_slice(b"0000000000 65535 f \n");
            for number in 1..object_count {
                match offsets.get(&(number as u32)) {
                    Some((generation, offset)) => bytes
                        .extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes()),
                    None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
                }
            }

            // Trailer — start from the document trailer, strip incremental keys.
            let mut trailer = pdf.trailer().clone();
            strip_incremental_trailer_keys(&mut trailer);
            // Remap surviving indirect trailer refs (notably /Info) to NEW
            // numbers. /Root is overwritten explicitly with new_root below, and
            // /Encrypt is handled by apply_encrypt_trailer_entries, so skip both.
            remap_trailer_refs(&mut trailer, &renumber, &skip_refs)?;
            trailer.insert("Size", Object::Integer(object_count as i64));
            trailer.insert("Root", Object::Reference(new_root));
            apply_encrypt_trailer_entries(
                &mut trailer,
                pdf,
                options,
                encrypt_ctx.as_ref(),
                options.deterministic_id,
            );

            if options.qdf {
                // qpdf --qdf trailer: "trailer <<" on one line, then one
                // "  /Key value" entry per line with the keys alphabetically
                // sorted but /ID forced LAST (verified empirically against
                // qpdf 11.9.0: minimal => /Root /Size /ID; three-page =>
                // /Info /Root /Size /ID). Values use the EXISTING compact
                // serializer, which keeps the /ID array inline
                // ("[<hex><hex>]") — do NOT route the trailer through the qdf
                // dict serializer. Closing ">>" then startxref directly (no
                // extra leading newline) to match the qpdf reference.
                if options.deterministic_id {
                    // Direct-write the real /ID inline (computed from the bytes
                    // written up to and including its opening `[`) instead of
                    // emitting the placeholder and byte-searching for it later.
                    // The bytes are identical to the placeholder-then-patch
                    // result for the same computed id, so output stays
                    // byte-for-byte equal to qpdf 11.9.0.
                    let mut id_writer = |out: &mut Vec<u8>| {
                        write_deterministic_id_inline(out, &det_id_info_suffix, det_id_source_id0)
                    };
                    write_qdf_trailer(&mut bytes, &trailer, Some(&mut id_writer));
                } else {
                    write_qdf_trailer(&mut bytes, &trailer, None);
                }
                bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
            } else {
                // qpdf classic trailer: the dict sits on the `trailer ` line
                // (single space, not its own line) with keys sorted but /ID
                // forced last — `trailer << /Info .. /Root .. /Size N /ID [..]
                // >>` (verified against qpdf 11.9.0 static-id goldens).
                bytes.extend_from_slice(b"trailer ");
                if options.deterministic_id {
                    // Direct-write the real /ID inline (computed from the bytes
                    // written up to and including its opening `[`) instead of
                    // emitting the placeholder and byte-searching for it later.
                    // The bytes are identical to the placeholder-then-patch
                    // result for the same computed id, so output stays
                    // byte-for-byte equal to qpdf 11.9.0.
                    let mut id_writer = |out: &mut Vec<u8>| {
                        write_deterministic_id_inline(out, &det_id_info_suffix, det_id_source_id0)
                    };
                    trailer.write_pdf_trailer(&mut bytes, Some(&mut id_writer));
                } else {
                    trailer.write_pdf_trailer(&mut bytes, None);
                }
                bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
            }
        }

        XrefForm::Stream => {
            // Cross-reference stream — pick a fresh object number beyond all
            // emitted objects.
            let xref_object_number = object_count as u32;

            // Build the xref stream entries.  We include every object number
            // in `[0, xref_object_number]` so that `/Size` and `/Index` stay
            // consistent: object 0 and any gaps in the emitted-objects set are
            // emitted as free entries, and the xref stream object itself sits
            // at the end.  This produces a single contiguous /Index range and
            // matches the structure a reader expects from a non-incremental
            // xref stream.
            let final_object_count = (xref_object_number as usize).saturating_add(1);
            let mut xref_entries: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::new();
            xref_entries.insert(0, (65535, XrefOffset::Free { next: 0 }));
            for number in 1..xref_object_number {
                if let Some(&(gen, off)) = offsets.get(&number) {
                    // Regular indirect object (plain or ObjStm container).
                    xref_entries.insert(
                        number,
                        (
                            gen,
                            XrefOffset::Offset(u64::try_from(off).map_err(|_| {
                                crate::Error::Unsupported(
                                    "xref offset does not fit u64".to_string(),
                                )
                            })?),
                        ),
                    );
                } else if let Some(&(container_num, index_in_batch)) =
                    member_new_to_batch.get(&number)
                {
                    // Member object that lives inside an ObjStm container.
                    // Compressed members have implicit generation 0; the
                    // type-2 xref record carries the container's object number
                    // and the member's index within that container.
                    xref_entries.insert(
                        number,
                        (
                            0,
                            XrefOffset::Compressed {
                                stream: container_num,
                                index: index_in_batch,
                            },
                        ),
                    );
                } else {
                    // Gap in emitted objects — emit as a free entry so the
                    // xref stream covers `[0, /Size)` contiguously.
                    xref_entries.insert(number, (0, XrefOffset::Free { next: 0 }));
                }
            }
            xref_entries.insert(
                xref_object_number,
                (
                    0,
                    XrefOffset::Offset(u64::try_from(xref_offset).map_err(|_| {
                        crate::Error::Unsupported("xref stream offset does not fit u64".to_string())
                    })?),
                ),
            );

            // The entries are now contiguous `[0, final_object_count)`, so a
            // single Index range suffices.
            let ranges: Vec<(u32, u32)> = vec![(0, final_object_count as u32)];

            // Build the raw xref entry bytes (binary format: field widths
            // declared in /W).  This is structural PDF data, not a content
            // stream, so `apply_stream_compress_policy` is not used here.
            // Instead we apply the compress_streams toggle directly: when
            // `CompressStreams::Yes` the raw bytes are FlateDecode-compressed
            // (matching qpdf's default behaviour for xref streams); when
            // `CompressStreams::No` they are stored without any filter.
            let raw_xref_bytes = build_xref_stream_bytes(&xref_entries, &ranges)?;

            let mut index_array = Vec::with_capacity(ranges.len() * 2);
            for &(start, count) in &ranges {
                index_array.push(Object::Integer(i64::from(start)));
                index_array.push(Object::Integer(i64::from(count)));
            }

            let mut xref_dict = pdf.trailer().clone();
            strip_incremental_trailer_keys(&mut xref_dict);
            // Remap surviving indirect trailer refs (notably /Info) to NEW
            // numbers. /Root is set explicitly to new_root below; /Encrypt is
            // handled by apply_encrypt_trailer_entries.
            remap_trailer_refs(&mut xref_dict, &renumber, &skip_refs)?;
            // The trailer may carry filter keys from the input's xref stream
            // (e.g. /Filter /FlateDecode). We're emitting freshly built bytes
            // via `Stream::new`, so any stale filter declaration would make
            // readers attempt to decode the new bytes under the wrong codec.
            // We set /Filter (or omit it) based on compress_streams below.
            xref_dict.remove("Filter");
            xref_dict.remove("DecodeParms");
            xref_dict.remove("F");
            xref_dict.remove("FFilter");
            xref_dict.remove("FDecodeParms");
            xref_dict.insert("Type", Object::Name(b"XRef".to_vec()));
            xref_dict.insert("Size", Object::Integer(final_object_count as i64));
            xref_dict.insert(
                "W",
                Object::Array(vec![
                    Object::Integer(1),
                    Object::Integer(8),
                    Object::Integer(4),
                ]),
            );
            xref_dict.insert("Index", Object::Array(index_array));
            xref_dict.insert("Root", Object::Reference(new_root));

            // Apply the compress_streams policy: FlateDecode-compress the xref
            // binary payload when Yes, or store raw bytes when No.
            let stream_data = match options.compress_streams {
                CompressStreams::Yes => {
                    let mut encode_dict = Dictionary::new();
                    encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                    match filters::encode_stream_data(&encode_dict, &raw_xref_bytes) {
                        Ok(compressed) => {
                            xref_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                            compressed
                        }
                        // Compression failure is essentially impossible for
                        // in-memory zlib, but fall back to raw bytes so we
                        // never emit an unreadable xref stream.
                        Err(_) => raw_xref_bytes,
                    }
                }
                CompressStreams::No => {
                    // No filter: store the structural binary directly.
                    raw_xref_bytes
                }
            };

            xref_dict.insert(
                "Length",
                Object::Integer(i64::try_from(stream_data.len()).map_err(|_| {
                    crate::Error::Unsupported("xref stream /Length does not fit i64".to_string())
                })?),
            );
            apply_encrypt_trailer_entries(
                &mut xref_dict,
                pdf,
                options,
                encrypt_ctx.as_ref(),
                options.deterministic_id,
            );

            // The cross-reference stream dictionary is serialized in plain
            // lexicographic order (so `/ID` is NOT last), and its compressed
            // binary payload follows. When deterministic-/ID is requested,
            // direct-write the real /ID inline at `/ID`'s sorted position,
            // computed from the bytes written up to and including its opening
            // `[` — the same digest range the placeholder-then-patch step used
            // to target. qpdf does not produce byte-parity output for xref-stream
            // form, but this content-derived /ID keeps the output self-stable.
            let xref_stream = crate::Stream::new(xref_dict, stream_data);
            bytes.extend_from_slice(format!("{xref_object_number} 0 obj\n").as_bytes());
            if options.deterministic_id {
                let mut id_writer = |out: &mut Vec<u8>| {
                    write_deterministic_id_inline(out, &det_id_info_suffix, det_id_source_id0)
                };
                write_stream_to_buf_with_id_writer(
                    &mut bytes,
                    &xref_stream,
                    options.newline_before_endstream,
                    Some(&mut id_writer),
                );
            } else {
                write_stream_to_buf(&mut bytes, &xref_stream, options.newline_before_endstream);
            }
            bytes.extend_from_slice(b"\nendobj\n");
            bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        }
    }

    out.write_all(&bytes)?;
    Ok(())
}

/// Non-linearized `--object-streams=generate`, byte-identical to qpdf 11.9.0.
///
/// qpdf assigns object streams up front (`QPDF::getCompressibleObjGens` DFS +
/// `QPDFWriter::generateObjectStreams` even split into `ceil(n/100)` containers),
/// then renumbers so each container is numbered immediately before its members,
/// members serialize in ascending source-object order, and reachable
/// uncompressed objects take the trailing numbers. The cross-reference is emitted
/// as a stream (type-2 entries require it) with the header floored to 1.5.
///
/// Scope: the plain (non-encrypt / non-copy-encryption / non-qdf) case; the
/// caller (`write_pdf_full_rewrite`) routes only those here.
///
/// # Errors
///
/// Returns [`crate::Error::Missing`] when the trailer has no `/Root`, refuses
/// signed inputs (a full rewrite invalidates signatures), and propagates load /
/// renumber / encode errors.
fn write_pdf_generate<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriteOptions,
) -> Result<()> {
    use crate::rewrite_renumber::{renumber_refs_in_place, GenerateRenumber};
    use std::collections::HashSet;

    // `ok_or` (eager) keeps the error construction on the covered happy path;
    // `Error::Missing` is a cheap `&'static str` variant, so no `or_fun_call`.
    let root_ref = pdf.root_ref().ok_or(crate::Error::Missing("/Root"))?;
    refuse_signed_full_rewrite(pdf, options)?;

    // ── object-stream assignment + generate-mode numbering ───────────────────
    // 1. getCompressibleObjGens DFS from the trailer => eligible list, ordered.
    let eligible = object_streams::compressible_objgens(pdf)?;
    // 2. generateObjectStreams even split => one group per container.
    let groups = object_streams::even_split_into_streams(&eligible);
    // 3. enqueueObject walk with the object-stream branch => container-first
    //    numbering (members ascending-source within each container).
    let renumber = GenerateRenumber::build(pdf, &groups)?;
    let new_root = renumber.new_for_original(root_ref).ok_or_else(|| {
        // cov:ignore-start: GenerateRenumber::build seeds /Root first, so it is
        // always mapped; this guards against a future build change.
        crate::Error::Unsupported("generate: /Root absent from renumber map".to_string())
        // cov:ignore-end
    })?;

    // ── per-container member tables + type-2 xref entries ────────────────────
    /// One ObjStm container: its assigned object number and its members as
    /// `(original, new)` ref pairs in ascending-NEW (= ascending-source) order.
    struct ContainerPlan {
        number: u32,
        members: Vec<(ObjectRef, ObjectRef)>,
    }
    // Member original refs (skipped from plain emission — they serialize inside
    // their container) and NEW member number -> (container number, index).
    let mut member_set: HashSet<ObjectRef> = HashSet::new();
    let mut member_xref: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    let mut containers: Vec<ContainerPlan> = Vec::with_capacity(groups.len());
    for (gi, group) in groups.iter().enumerate() {
        let number = renumber.container_number(gi).ok_or_else(|| {
            // cov:ignore-start: every group's members come from
            // `compressible_objgens` (all reachable), so each group is numbered.
            crate::Error::Unsupported(
                "generate: ObjStm container group was never reached".to_string(),
            )
            // cov:ignore-end
        })?;
        // Resolve each member's NEW ref once. qpdf serializes a container's
        // members in ascending source-object order (`std::set<QPDFObjGen>`), and
        // the generate-mode numbering assigns new numbers in that same order — so
        // sorting by the NEW number reproduces it.
        let mut members: Vec<(ObjectRef, ObjectRef)> = group
            .iter()
            .map(|&old| {
                renumber
                    .new_for_original(old)
                    .map(|new| (old, new))
                    .ok_or_else(|| {
                        // cov:ignore-start: members come from the same walk that
                        // built the map, so each is present.
                        crate::Error::Unsupported(
                            "generate: ObjStm member absent from renumber map".to_string(),
                        )
                        // cov:ignore-end
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        members.sort_by_key(|&(_, new)| new.number);
        for (index, &(old, new)) in members.iter().enumerate() {
            member_set.insert(old);
            let index = u32::try_from(index).map_err(|_| {
                // cov:ignore-start: a single ObjStm holds at most 100 members,
                // far below u32::MAX.
                crate::Error::Unsupported("generate: ObjStm member index overflows u32".to_string())
                // cov:ignore-end
            })?;
            member_xref.insert(new.number, (number, index));
        }
        containers.push(ContainerPlan { number, members });
    }

    // Emission dispatch keyed on NEW object number: a container body or a plain
    // (uncompressed) object. Compressed members are skipped here. qpdf writes
    // objects in ascending new number, so iterating the BTreeMap drives the body
    // order directly.
    enum Emit {
        Container(usize),
        Plain(ObjectRef),
    }
    let mut emit: BTreeMap<u32, Emit> = BTreeMap::new();
    for (new, old) in renumber.pairs() {
        if member_set.contains(&old) {
            continue;
        }
        emit.insert(new.number, Emit::Plain(old));
    }
    for (gi, c) in containers.iter().enumerate() {
        if emit.insert(c.number, Emit::Container(gi)).is_some() {
            // cov:ignore-start: container numbers and plain-object numbers are
            // disjoint by construction (members are excluded above).
            return Err(crate::Error::Unsupported(
                "generate: container object number collides with a plain object".to_string(),
            ));
            // cov:ignore-end
        }
    }

    // ── header ───────────────────────────────────────────────────────────────
    // Pass `object_streams = true`: xref streams require PDF 1.5, and
    // `effective_pdf_version` applies that floor (above any lower source /
    // --force-version), so no separate clamp is needed here.
    let version = effective_pdf_version(pdf.version(), options, false, true).to_owned();

    // deterministic-/ID seed inputs, captured from the ORIGINAL trailer before
    // the emit loop borrows `pdf` (qpdf reads these from the source trailer).
    let (det_id_source_id0, det_id_info_suffix): (Option<[u8; 16]>, Vec<u8>) =
        if options.deterministic_id {
            (
                source_permanent_id(pdf.trailer()),
                deterministic_id_info_suffix(pdf),
            )
        } else {
            (None, Vec::new())
        };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);

    // ── bodies in ascending new number ───────────────────────────────────────
    // `offsets` records the byte offset of every emitted body (containers and
    // plain objects). Members are absent (they live inside containers) and get
    // type-2 xref entries from `member_xref` instead.
    let mut offsets: BTreeMap<u32, usize> = BTreeMap::new();
    for (&number, what) in &emit {
        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        match what {
            Emit::Plain(old) => {
                let mut object = pdf.resolve(*old)?;
                renumber_refs_in_place(&mut object, &renumber)?;
                if let Object::Stream(stream) = object {
                    let (reencoded, source_filter_is_lone_flate) =
                        reencode_stream_for_compress(stream, options);
                    write_reencoded_object(
                        &mut bytes,
                        &reencoded,
                        source_filter_is_lone_flate,
                        options,
                    );
                } else {
                    object.write_pdf(&mut bytes);
                }
            }
            Emit::Container(gi) => {
                // Resolve each member, rewrite its internal references to NEW
                // numbers, and pair with its (precomputed) NEW ref so the ObjStm
                // pair table records the renumbered member number.
                let plan = &containers[*gi];
                let mut resolved: Vec<(ObjectRef, Object)> = Vec::with_capacity(plan.members.len());
                for &(old, new) in &plan.members {
                    let mut obj = pdf.resolve(old)?;
                    renumber_refs_in_place(&mut obj, &renumber)?;
                    resolved.push((new, obj));
                }
                let body = object_streams::emit_objstm_body_from_resolved(&resolved)?;
                let stream = object_streams::wrap_objstm_body(&body, options.compress_streams)?;
                // Emit the container dict in qpdf 11.9.0's fixed key order
                // (`/Type /ObjStm /Length L [/Filter /FlateDecode] /N n /First f`);
                // the BTreeMap-backed `Object::Stream` serializer would
                // alphabetize the keys instead. `/Filter` is present iff the body
                // was compressed (`CompressStreams::Yes`).
                bytes.extend_from_slice(b"<< /Type /ObjStm /Length ");
                bytes.extend_from_slice(stream.data.len().to_string().as_bytes());
                if stream.dict.get("Filter").is_some() {
                    bytes.extend_from_slice(b" /Filter /FlateDecode");
                }
                bytes.extend_from_slice(
                    format!(" /N {} /First {} >>", body.n_members, body.first_offset).as_bytes(),
                );
                write_stream_payload(&mut bytes, &stream.data, options.newline_before_endstream);
            }
        }
        bytes.extend_from_slice(b"\nendobj\n");
        offsets.insert(number, emit_offset);
    }

    // ── cross-reference stream ───────────────────────────────────────────────
    // qpdf emits a PNG-Up-predicted (`/Predictor 12`), minimal-`/W`,
    // fixed-key-order xref stream. Reuse the linearized encoder, which already
    // reproduces that shape byte-for-byte (the generic `build_xref_stream_bytes`
    // uses `/W [1 8 4]` without a predictor and is NOT byte-identical to qpdf).
    use crate::linearization::xref_stream;
    let xref_offset = bytes.len();
    // Highest object number across plain bodies, containers, AND compressed
    // members. Numbering is contiguous `1..=M`; the xref stream itself takes
    // `M + 1` and `/Size` is `M + 2`.
    let max_object_number = offsets
        .keys()
        .chain(member_xref.keys())
        .copied()
        .max()
        .unwrap_or(0);
    let xref_object_number = max_object_number.checked_add(1).ok_or_else(|| {
        // cov:ignore-start: would require ~u32::MAX live objects (a multi-GB PDF).
        crate::Error::Unsupported("generate: xref stream object number overflows u32".to_string())
        // cov:ignore-end
    })?;
    let size = xref_object_number.checked_add(1).ok_or_else(|| {
        // cov:ignore-start: see above — unreachable below u32::MAX objects.
        crate::Error::Unsupported("generate: xref /Size overflows u32".to_string())
        // cov:ignore-end
    })?;

    // The xref stream object describes itself with a type-1 entry at its own
    // offset; add it to the offset map so `build_entries` emits that row.
    let mut offs = offsets;
    offs.insert(xref_object_number, xref_offset);

    let entries = xref_stream::build_entries(&offs, &member_xref, 0, size);
    // Minimal `/W` widths: field 2 sizes the largest type-1 byte offset (no hint
    // stream, so `hint_length = 0`), field 3 the largest ObjStm member index.
    // Single xref => no `/Prev` chain and no two-pass padding.
    let max_offset = xref_stream::max_entry_offset(&entries);
    let max_ostream_index = member_xref
        .values()
        .map(|&(_, index)| u64::from(index))
        .max()
        .unwrap_or(0);
    let widths =
        xref_stream::second_pass_widths(max_offset, 0, max_object_number, max_ostream_index);
    let payload = xref_stream::encode_payload(&entries, widths);

    // Trailer-derived dict entries: /Info (remapped to its new number) and the
    // two-element /ID. /Root is the renumbered catalog. qpdf omits /Index on a
    // single full-range xref stream.
    let skip_refs = pdf.deleted_object_refs();
    let mut trailer = pdf.trailer().clone();
    strip_incremental_trailer_keys(&mut trailer);
    remap_trailer_refs(&mut trailer, &renumber, &skip_refs)?;
    let info_ref = match trailer.get("Info") {
        Some(Object::Reference(r)) => Some(*r),
        _ => None,
    };

    let xref_ref = ObjectRef::new(xref_object_number, 0);
    if options.deterministic_id {
        // qpdf does not produce byte-parity output for xref-stream form, but the
        // content-derived /ID must be self-stable. Write it INLINE at the /ID
        // position so the digest covers the bytes up to the array's `[` —
        // matching `compute_deterministic_id`'s contract on every other path.
        let dict = xref_stream::XrefStreamDict {
            widths,
            index: None,
            info: info_ref,
            root: Some(new_root),
            size,
            prev: None,
            id: None,
        };
        let mut id_writer = |out: &mut Vec<u8>| {
            write_deterministic_id_inline(out, &det_id_info_suffix, det_id_source_id0)
        };
        xref_stream::write_object_with_id_writer(
            &mut bytes,
            xref_ref,
            &dict,
            &payload,
            &mut id_writer,
        );
    } else {
        // static => preserved source /ID[0] (or the pi constant when absent) + pi
        // /ID[1]; random otherwise. Both flow through
        // `apply_encrypt_trailer_entries`, so this path and the full-rewrite path
        // agree on the /ID bytes.
        let mut id_trailer = Dictionary::new();
        if let Some(id) = pdf.trailer().get("ID") {
            id_trailer.insert("ID", id.clone());
        }
        apply_encrypt_trailer_entries(&mut id_trailer, pdf, options, None, false);
        // `apply_encrypt_trailer_entries` always sets /ID to a two-element array
        // of strings here (apply_static_id / apply_random_id), so the non-String
        // / non-pair fallbacks below are defensive only.
        let (id0, id1): (Vec<u8>, Vec<u8>) = match id_trailer.get("ID") {
            Some(Object::Array(arr)) if arr.len() == 2 => {
                let take = |o: &Object| match o {
                    Object::String(s) => s.clone(),
                    _ => QPDF_STATIC_ID.to_vec(), // cov:ignore: /ID elements are always strings here
                };
                (take(&arr[0]), take(&arr[1]))
            }
            _ => (QPDF_STATIC_ID.to_vec(), QPDF_STATIC_ID.to_vec()), // cov:ignore: /ID is always a 2-string array here
        };
        let dict = xref_stream::XrefStreamDict {
            widths,
            index: None,
            info: info_ref,
            root: Some(new_root),
            size,
            prev: None,
            id: Some((&id0, &id1)),
        };
        xref_stream::write_object(&mut bytes, xref_ref, &dict, &payload);
    }
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    out.write_all(&bytes)?;
    Ok(())
}

/// Apply the stream compression policy to a single stream object.
///
/// This is the choke-point for re-emitting **regular indirect stream
/// objects** in the full-rewrite path. The cross-reference stream and
/// object-stream (ObjStm) containers apply the same `CompressStreams`
/// policy on their own dedicated branches (the xref-stream branch below
/// and `object_streams::wrap_objstm_body`); they do not flow through
/// this function. (`write_qdf` is exempt — it is a debug-dump path that
/// emits source bytes verbatim by design and does not consult this function.)
///
/// # Policy: `CompressStreams::Yes` (default)
///
/// Decode the stream through its declared filter pipeline and re-encode with a
/// single `/FlateDecode` filter.  This matches qpdf's default passthrough mode.
///
/// Streams whose decode succeeds but re-encode fails (vanishingly rare for
/// in-memory zlib) are returned verbatim.
///
/// # Policy: `CompressStreams::No`
///
/// Decode the stream and emit the raw bytes without any `/Filter`.  The
/// filter-related keys (`/Filter`, `/DecodeParms`, `/F`, `/FFilter`,
/// `/FDecodeParms`) are stripped from the output dictionary.
///
/// # Fallback for unsupported / corrupt inputs
///
/// When `decode_stream_data` returns an error — e.g. because the declared
/// filter is `DCTDecode` or `JPXDecode` (image codecs not implemented by
/// flpdf) or because the stream data is corrupt — the original stream is
/// returned **unchanged** (dict + data verbatim).  This preserves readability:
/// a PDF reader that understands the codec can still decode the stream, and we
/// do not corrupt the data by emitting uninterpreted bytes under a wrong
/// (or missing) filter declaration.
///
/// # Byte-vs-observable note
///
/// For `CompressStreams::Yes`, flpdf's FlateDecode output uses
/// `flate2::Compression::default()`, which selects different compression
/// parameters than qpdf's internal zlib build.  The decoded bytes are
/// identical to qpdf's, but the raw compressed bytes differ.  This is
/// intentional: byte-identical agreement with qpdf is not a goal for this
/// toggle.
/// See [`CompressStreams`] for the full policy statement.
pub fn apply_stream_compress_policy(stream: &crate::Stream, policy: CompressStreams) -> Object {
    // Decode the stream through whatever filters are declared in its dict.
    let decoded = match filters::decode_stream_data(&stream.dict, &stream.data) {
        Ok(d) => d,
        Err(_) => {
            // Decode failure (unsupported codec or corrupt data): emit verbatim.
            // The original /Filter chain is preserved so downstream readers
            // (e.g. image renderers) can still interpret the stream correctly.
            return Object::Stream(stream.clone());
        }
    };

    // Build a new dict: strip all filter-related keys, update /Length.
    // `/F` carries an external-file reference for the stream data, so we
    // strip it as well — otherwise readers may try to load the old external
    // file instead of the new embedded stream we just produced.
    let mut new_dict = stream.dict.clone();
    new_dict.remove("Filter");
    new_dict.remove("DecodeParms");
    new_dict.remove("F");
    new_dict.remove("FFilter");
    new_dict.remove("FDecodeParms");

    match policy {
        CompressStreams::Yes => {
            // Re-encode with a minimal FlateDecode dict.  If encoding fails
            // (vanishingly rare for in-memory zlib), keep the original stream
            // verbatim — declaring /FlateDecode on uncompressed bytes would
            // produce an unreadable PDF.
            let mut encode_dict = Dictionary::new();
            encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            let encoded = match filters::encode_stream_data(&encode_dict, &decoded) {
                Ok(e) => e,
                Err(_) => return Object::Stream(stream.clone()),
            };

            // Always apply FlateDecode — even if the encoded result is larger
            // than the raw data (which can happen for small streams).  This
            // guarantees a single well-known filter regardless of stream size.
            new_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            new_dict.insert(
                "Length",
                Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
            );
            Object::Stream(crate::Stream::new(new_dict, encoded))
        }
        CompressStreams::No => {
            // Emit raw (decoded) bytes without any filter.
            new_dict.insert(
                "Length",
                Object::Integer(i64::try_from(decoded.len()).unwrap_or(i64::MAX)),
            );
            Object::Stream(crate::Stream::new(new_dict, decoded))
        }
    }
}

/// Whether a stream's `/Filter` value is a lone `/FlateDecode` — either the
/// bare name `/FlateDecode` or a single-element array `[ /FlateDecode ]`. PDF
/// permits both forms (ISO 32000-1 §7.3.8.2), and qpdf preserves such streams
/// (does not re-filter them), so the stream-dict key ordering must treat both
/// the same way.
pub(crate) fn is_lone_flate(filter: Option<&Object>) -> bool {
    match filter {
        Some(Object::Name(name)) => name.as_slice() == b"FlateDecode",
        Some(Object::Array(items)) => {
            matches!(items.as_slice(), [Object::Name(name)] if name.as_slice() == b"FlateDecode")
        }
        _ => false,
    }
}

/// Write a PDF stream to `buf`, applying the [`NewlineBeforeEndstream`] policy.
///
/// This is the **single choke-point** through which all stream emission in the
/// full-rewrite writer paths flows.  It mirrors the layout that
/// `Object::Stream::write_pdf` produces, but gives the caller control over
/// the newline before `endstream`.
///
/// # Layout
///
/// ```text
/// <stream-dict>\nstream\n<payload><EOL>endstream
/// ```
///
/// where `<EOL>` is:
/// - `NewlineBeforeEndstream::Yes`: always `b'\n'` (one byte, unconditionally).
/// - `NewlineBeforeEndstream::No`: empty when payload ends with `\n` or `\r`;
///   otherwise `b'\n'` (one byte, for ISO 32000-1 parseability).
///
/// # /Length invariant
///
/// The helper **does not** modify the stream dictionary.  Callers are
/// responsible for setting `/Length` to `stream.data.len()` before calling
/// (i.e., the raw payload byte count, not including the EOL byte).
pub fn write_stream_to_buf(
    buf: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
) {
    stream.dict.write_pdf(buf);
    write_stream_payload(buf, &stream.data, policy);
}

/// Like [`write_stream_to_buf`] but serializes the stream dictionary via
/// [`crate::object::Dictionary::write_pdf_with_id_writer`], so the `/ID` value
/// (at its lexicographic position in the dictionary) can be produced by
/// `id_writer` from the bytes written so far. With `id_writer = None` the output
/// is byte-identical to [`write_stream_to_buf`]. Used by the cross-reference
/// stream path to direct-write the deterministic `/ID` inline instead of
/// emitting a placeholder and byte-searching for it afterwards.
pub(crate) fn write_stream_to_buf_with_id_writer(
    buf: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
    id_writer: Option<crate::object::TrailerIdWriter>,
) {
    stream.dict.write_pdf_with_id_writer(buf, id_writer);
    write_stream_payload(buf, &stream.data, policy);
}

/// Like [`write_stream_to_buf`] but serializes the stream dictionary in qpdf's
/// stream-dictionary key order (see [`crate::object::Dictionary::write_pdf_stream`]):
/// `/Length` is pulled out and written after the other (sorted) keys, and when
/// `refiltered` is set, `/Filter`/`/DecodeParms` are dropped from the iteration
/// and `/Filter /FlateDecode` is re-appended after `/Length`. Used by the
/// full-rewrite path so re-encoded content streams match `qpdf --static-id`
/// byte-for-byte (modulo the deflate-backend-dependent `/Length` value).
pub(crate) fn write_stream_to_buf_qpdf_order(
    buf: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
    refiltered: bool,
) {
    stream.dict.write_pdf_stream(buf, refiltered);
    write_stream_payload(buf, &stream.data, policy);
}

/// Emit a preserved (verbatim) stream body: the `dict` in qpdf's stream-dict key
/// order (`/Length` pulled out and written last; no re-filtering) followed by the
/// raw `data` framed with no newline before `endstream`. Used to emit an
/// already-lone-/FlateDecode stream without decode + re-encode. The caller is
/// responsible for setting `dict`'s `/Length` to `data.len()`.
pub(crate) fn write_preserved_stream(buf: &mut Vec<u8>, dict: &Dictionary, data: &[u8]) {
    dict.write_pdf_stream(buf, false);
    write_stream_payload(buf, data, NewlineBeforeEndstream::Never);
}

/// Emit the `\nstream\n<payload><EOL>endstream` framing shared by
/// [`write_stream_to_buf`] and [`write_stream_to_buf_qpdf_order`], applying the
/// [`NewlineBeforeEndstream`] policy. The dictionary must already be written.
fn write_stream_payload(buf: &mut Vec<u8>, data: &[u8], policy: NewlineBeforeEndstream) {
    buf.extend_from_slice(b"\nstream\n");
    buf.extend_from_slice(data);

    match policy {
        NewlineBeforeEndstream::Yes => {
            // Always write exactly one newline before endstream.
            buf.push(b'\n');
        }
        NewlineBeforeEndstream::No => {
            // Only write a newline when the payload does not already end with one.
            let ends_with_eol = data
                .last()
                .map(|&b| b == b'\n' || b == b'\r')
                .unwrap_or(false);
            if !ends_with_eol {
                buf.push(b'\n');
            }
        }
        NewlineBeforeEndstream::Never => {
            // Write nothing: endstream is adjacent to the raw payload (qpdf
            // default output).
        }
    }

    buf.extend_from_slice(b"endstream");
}

/// On-disk byte count of a stream payload as written by
/// [`write_stream_to_buf`] / [`write_stream_to_buf_qdf`] for the given
/// [`NewlineBeforeEndstream`] policy: the raw `data` length plus the single
/// EOL byte the writer inserts before `endstream` (if any).
///
/// This is the value ISO 32000-1 §7.3.8.1 mandates for `/Length` (bytes
/// between the `stream` EOL and the `endstream` keyword) and is exactly the
/// count [`crate::fix_qdf`] (qdf_fix.rs) recomputes from the emitted bytes.
/// In QDF mode the indirect length-holder body MUST equal this — not the
/// raw decoded length — so the writer and `fix_qdf` mesh.
fn on_disk_stream_len(data: &[u8], policy: NewlineBeforeEndstream) -> usize {
    let n = data.len();
    match policy {
        NewlineBeforeEndstream::Yes => n + 1,
        NewlineBeforeEndstream::No => {
            let ends_with_eol = data
                .last()
                .map(|&b| b == b'\n' || b == b'\r')
                .unwrap_or(false);
            if ends_with_eol {
                n
            } else {
                n + 1
            }
        }
        // No EOL is ever added, so the on-disk length is exactly the payload.
        NewlineBeforeEndstream::Never => n,
    }
}

/// QDF variant of [`write_stream_to_buf`]: identical stream/endstream framing
/// and newline-before-endstream behaviour, but the stream dictionary is
/// serialized with the qpdf `--qdf` multi-line / sorted-key layout
/// ([`Dictionary::write_pdf_qdf`]) instead of the compact form. Used only on
/// the qdf full-rewrite path; preserves the 6.1-era stream invariants
/// (raw `data`, `/Length` already correct in `dict`).
fn write_stream_to_buf_qdf(
    buf: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
) {
    stream.dict.write_pdf_qdf(buf, 0);
    // Stream framing + newline-before-endstream policy is identical to the
    // compact path; only the dict serialization differs in qdf mode.
    write_stream_payload(buf, &stream.data, policy);
}

/// Emit the rebuilt full-rewrite trailer in qpdf `--qdf` formatting:
///
/// ```text
/// trailer <<
///   /Key value
///   ...
/// >>
/// ```
///
/// Keys are emitted alphabetically by raw name, **except `/ID` which is forced
/// last** — this matches qpdf 11.9.0 exactly (minimal.pdf: `/Root /Size /ID`;
/// three-page.pdf: `/Info /Root /Size /ID`). Each value is written with the
/// EXISTING compact [`Object::write_pdf`] serializer, so array values such as
/// `/ID [<hex><hex>]` stay inline (qpdf formats the trailer specially). The
/// closing `>>` is followed by a newline; the caller appends `startxref`
/// directly afterwards.
///
/// When `id_writer` is `Some`, the `/ID` *value* is produced by that closure
/// (the `  /ID ` key token is still emitted) instead of serializing the
/// dictionary's stored `/ID` value. This lets the caller compute the `/ID`
/// directly from the bytes written so far — used by the deterministic-`/ID`
/// writer to emit qpdf's content-derived identifier inline rather than via a
/// placeholder-then-patch step. The closure runs only when the `/ID` key is
/// present in the dictionary; if it is absent, `id_writer` is ignored.
fn write_qdf_trailer(
    bytes: &mut Vec<u8>,
    trailer: &Dictionary,
    id_writer: Option<crate::object::TrailerIdWriter>,
) {
    bytes.extend_from_slice(b"trailer <<\n");

    // `Dictionary::iter()` already yields keys in lexicographic (BTreeMap)
    // order; split out /ID so it can be appended last.
    let mut id_value: Option<&Object> = None;
    for (key, value) in trailer.iter() {
        if key == b"ID" {
            id_value = Some(value);
            continue;
        }
        bytes.extend_from_slice(b"  /");
        crate::object::write_name_escaped(bytes, key);
        bytes.push(b' ');
        value.write_pdf(bytes);
        bytes.push(b'\n');
    }
    if let Some(value) = id_value {
        bytes.extend_from_slice(b"  /ID ");
        match id_writer {
            Some(write_id) => write_id(bytes),
            None => value.write_pdf(bytes),
        }
        bytes.push(b'\n');
    }

    bytes.extend_from_slice(b">>\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite_renumber::CatalogFirstRenumber;

    #[test]
    fn remap_trailer_refs_remaps_live_and_drops_deleted() {
        // /Info points at a live object (10 -> new 3); /Meta points at a
        // deleted object (20). The live ref must be remapped; the deleted ref's
        // key must be dropped (not remapped to a free xref row). /Root and
        // /Encrypt are left for the caller and must be untouched here.
        let map = CatalogFirstRenumber::from_pairs_for_test(&[
            (ObjectRef::new(1, 0), ObjectRef::new(1, 0)),
            (ObjectRef::new(10, 0), ObjectRef::new(3, 0)),
        ]);
        let mut trailer = Dictionary::new();
        trailer.insert("Root", Object::Reference(ObjectRef::new(1, 0)));
        trailer.insert("Info", Object::Reference(ObjectRef::new(10, 0)));
        trailer.insert("Meta", Object::Reference(ObjectRef::new(20, 0)));
        trailer.insert("Size", Object::Integer(4));

        let deleted = [ObjectRef::new(20, 0)];
        remap_trailer_refs(&mut trailer, &map, &deleted).expect("remap");

        assert_eq!(
            trailer.get("Info"),
            Some(&Object::Reference(ObjectRef::new(3, 0))),
            "live /Info must be remapped to its new number"
        );
        assert!(
            trailer.get("Meta").is_none(),
            "/Meta pointing at a deleted object must be dropped, not remapped"
        );
        // /Root is filtered from remapping (caller owns it) and stays as-is.
        assert_eq!(
            trailer.get("Root"),
            Some(&Object::Reference(ObjectRef::new(1, 0)))
        );
    }

    #[test]
    fn remap_trailer_refs_errors_on_unmapped_live_ref() {
        // A non-deleted trailer ref absent from the map is a real
        // inconsistency and must surface as an error, not a stale number.
        let map = CatalogFirstRenumber::from_pairs_for_test(&[(
            ObjectRef::new(1, 0),
            ObjectRef::new(1, 0),
        )]);
        let mut trailer = Dictionary::new();
        trailer.insert("Info", Object::Reference(ObjectRef::new(99, 0)));
        let err = remap_trailer_refs(&mut trailer, &map, &[]).unwrap_err();
        assert!(matches!(err, crate::Error::Unsupported(_)));
    }

    fn id_array(d: &Dictionary) -> Vec<Object> {
        match d.get("ID") {
            Some(Object::Array(v)) => v.clone(),
            other => panic!("expected /ID array, got {other:?}"),
        }
    }

    #[test]
    fn apply_static_id_preserves_first_when_both_elements_are_strings() {
        let mut t = Dictionary::new();
        t.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"permanent".to_vec()),
                Object::String(b"changing".to_vec()),
            ]),
        );
        apply_static_id(&mut t);
        let v = id_array(&t);
        assert_eq!(
            v[0],
            Object::String(b"permanent".to_vec()),
            "element 1 must be preserved when /ID is a 2-string array"
        );
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    #[test]
    fn apply_static_id_falls_back_when_second_element_is_not_a_string() {
        // `/ID [<valid> 123]` — arity 2 but element 2 is not a string.
        // Both elements must fall back to the constant (qpdf parity); the
        // old guard checked only element 1 and wrongly kept it.
        let mut t = Dictionary::new();
        t.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"permanent".to_vec()),
                Object::Integer(123),
            ]),
        );
        apply_static_id(&mut t);
        let v = id_array(&t);
        assert_eq!(
            v[0],
            Object::String(QPDF_STATIC_ID.to_vec()),
            "malformed second element must force element 1 to the constant"
        );
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    #[test]
    fn apply_static_id_falls_back_when_id_is_missing() {
        let mut t = Dictionary::new();
        apply_static_id(&mut t);
        let v = id_array(&t);
        assert_eq!(v[0], Object::String(QPDF_STATIC_ID.to_vec()));
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    // --- Default (no-flag) /ID generation strategy (flpdf-9hc.13.2) ---------

    fn str_bytes(o: &Object) -> &[u8] {
        match o {
            Object::String(s) => s.as_slice(),
            other => panic!("expected /ID element to be a string, got {other:?}"),
        }
    }

    #[test]
    fn apply_random_id_first_save_generates_both_elements_fresh() {
        // No source /ID: both elements must be fresh 16-byte randoms — never
        // the π constant and never all-zero.
        let mut t = Dictionary::new();
        apply_random_id(&mut t);
        let v = id_array(&t);
        assert_eq!(v.len(), 2);
        let (e0, e1) = (str_bytes(&v[0]), str_bytes(&v[1]));
        assert_eq!(e0.len(), 16, "element 1 must be 16 bytes");
        assert_eq!(e1.len(), 16, "element 2 must be 16 bytes");
        assert_ne!(
            e0,
            &QPDF_STATIC_ID[..],
            "element 1 must not be the π constant"
        );
        assert_ne!(
            e1,
            &QPDF_STATIC_ID[..],
            "element 2 must not be the π constant"
        );
        assert!(e0.iter().any(|&b| b != 0), "element 1 must not be all-zero");
        assert!(e1.iter().any(|&b| b != 0), "element 2 must not be all-zero");
        // First save: the two elements are independently random.
        assert_ne!(
            e0, e1,
            "first-save elements must be independently generated"
        );
    }

    #[test]
    fn apply_random_id_varies_per_save() {
        // Two saves of the same (no-/ID) input yield different /ID arrays —
        // even back-to-back, thanks to the process-global counter.
        let mut a = Dictionary::new();
        let mut b = Dictionary::new();
        apply_random_id(&mut a);
        apply_random_id(&mut b);
        assert_ne!(id_array(&a), id_array(&b), "/ID must change on every save");
    }

    #[test]
    fn apply_random_id_re_save_preserves_element1_rotates_element2() {
        // First save (no source /ID): both fresh.
        let mut first = Dictionary::new();
        apply_random_id(&mut first);
        let v1 = id_array(&first);
        let perm = v1[0].clone();

        // Re-save: feed the well-formed 2-string /ID back in.  Element 1 must
        // be preserved verbatim (ISO 32000-1 §14.4); element 2 must rotate.
        let mut second = Dictionary::new();
        second.insert("ID", Object::Array(v1.clone()));
        apply_random_id(&mut second);
        let v2 = id_array(&second);

        assert_eq!(
            v2[0], perm,
            "element 1 (permanent id) must be preserved on re-save"
        );
        assert_ne!(
            v2[1], v1[1],
            "element 2 (changing id) must rotate on re-save"
        );
    }

    #[test]
    fn apply_random_id_regenerates_element1_when_source_id_is_malformed() {
        // Arity-2 array but element 2 is not a string → not a usable /ID, so
        // element 1 is treated as a first save and regenerated.
        let mut t = Dictionary::new();
        t.insert(
            "ID",
            Object::Array(vec![
                Object::String(b"would-be-permanent".to_vec()),
                Object::Integer(123),
            ]),
        );
        apply_random_id(&mut t);
        let v = id_array(&t);
        assert_ne!(
            v[0],
            Object::String(b"would-be-permanent".to_vec()),
            "malformed source /ID must not be trusted as permanent id"
        );
        assert_eq!(str_bytes(&v[0]).len(), 16);
        assert_eq!(str_bytes(&v[1]).len(), 16);
    }

    // --- deterministic-id (qpdf --deterministic-id) -----------------------

    fn det_id_options() -> WriteOptions {
        WriteOptions {
            full_rewrite: true,
            deterministic_id: true,
            ..WriteOptions::default()
        }
    }

    fn write_det_id(fixture: &[u8]) -> Vec<u8> {
        let mut pdf = crate::Pdf::open_mem(fixture).expect("fixture must open");
        let mut out = Vec::new();
        write_pdf_with_options(&mut pdf, &mut out, &det_id_options())
            .expect("deterministic-id write must succeed");
        out
    }

    fn trailer_id_pair(output: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let pdf = crate::Pdf::open_mem(output).expect("output must re-open");
        let id = pdf
            .trailer()
            .get("ID")
            .and_then(Object::as_array)
            .expect("trailer /ID must be an array");
        let extract = |o: &Object| {
            o.as_string()
                .expect("/ID element must be a string")
                .to_vec()
        };
        (extract(&id[0]), extract(&id[1]))
    }

    /// Locate the LAST `/ID [` array in an output PDF and return the byte
    /// offset of its opening `[`. qpdf captures the running content digest at
    /// exactly this point — inclusive of the `[` — so `md5(output[..=offset])`
    /// is the deterministic-id seed's `det_data` value.
    fn id_array_bracket_offset(output: &[u8]) -> usize {
        let id = b"/ID";
        let id_pos = output
            .windows(id.len())
            .rposition(|w| w == id)
            .expect("output must contain /ID");
        id_pos
            + output[id_pos..]
                .iter()
                .position(|&b| b == b'[')
                .expect("/ID must be followed by an array")
    }

    /// Re-derive qpdf's two-level deterministic `/ID[1]` (changing identifier)
    /// from an output PDF and the expected `/Info` seed suffix. This mirrors
    /// `compute_deterministic_id` so a wrong digest range, seed order, or
    /// `/Info` handling in the writer would make the assertion fail. The seed is
    /// truncated at its first NUL byte before the final hash, matching qpdf's
    /// `encodeString(seed.c_str())` strlen behaviour.
    fn expected_changing_id(output: &[u8], info_suffix: &[u8]) -> [u8; 16] {
        use md5::Digest as _;
        let bracket = id_array_bracket_offset(output);
        let det_data = md5::Md5::digest(&output[..=bracket]);
        let mut seed = Vec::new();
        for byte in det_data.iter() {
            seed.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        seed.extend_from_slice(b" QPDF ");
        seed.extend_from_slice(info_suffix);
        let truncated = &seed[..seed.iter().position(|&b| b == 0).unwrap_or(seed.len())];
        md5::Md5::digest(truncated).into()
    }

    #[test]
    fn deterministic_id_is_stable_and_matches_two_level_md5() {
        let fixture = build_string_and_stream_fixture();
        let o1 = write_det_id(&fixture);
        let o2 = write_det_id(&fixture);
        assert_eq!(
            o1, o2,
            "same input + deterministic_id must produce byte-identical output"
        );

        // This fixture's /Info carries /Title (TopSecretTitle), so the seed is
        // det_data + " QPDF " + " TopSecretTitle". /ID[1] is the two-level MD5;
        // with no source /ID the permanent identifier /ID[0] equals it.
        let (id0, id1) = trailer_id_pair(&o1);
        let expected = expected_changing_id(&o1, b" TopSecretTitle").to_vec();
        assert_eq!(
            id1, expected,
            "/ID[1] must be md5(det_data + \" QPDF \" + \" TopSecretTitle\")"
        );
        assert_eq!(id0, id1, "absent source /ID makes /ID[0] equal /ID[1]");
        // Distinct from the --static-id constant so the two flags never collide.
        assert_ne!(id0.as_slice(), &QPDF_STATIC_ID[..]);
    }

    #[test]
    fn deterministic_id_depends_on_content() {
        let a = write_det_id(&build_string_and_stream_fixture());
        let b = write_det_id(&build_metadata_fixture());
        assert_ne!(
            trailer_id_pair(&a).0,
            trailer_id_pair(&b).0,
            "different input content must yield a different deterministic /ID"
        );
    }

    /// Build a minimal classic-xref PDF whose trailer carries the given extra
    /// keys (e.g. `/ID [..]` or `/Info N 0 R`). `extra_objects` is appended
    /// verbatim as additional indirect objects (object numbers start at 3).
    fn build_det_id_source(trailer_extra: &str, extra_objects: &[&str]) -> Vec<u8> {
        let mut src = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        for obj in extra_objects {
            offsets.push(src.len());
            src.extend_from_slice(obj.as_bytes());
        }
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R {trailer_extra} >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        src
    }

    #[test]
    fn deterministic_id_preserves_source_permanent_id() {
        // A source with a well-formed 16-byte /ID: /ID[0] (permanent
        // identifier) must be preserved; only /ID[1] (changing identifier)
        // becomes the two-level digest.
        let src = build_det_id_source(
            &format!("/ID [<{}><{}>]", "aa".repeat(16), "bb".repeat(16)),
            &[],
        );
        let out = write_det_id(&src);
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(
            id0,
            vec![0xAAu8; 16],
            "/ID[0] must be preserved from the source"
        );
        assert_eq!(
            id1,
            expected_changing_id(&out, b"").to_vec(),
            "/ID[1] must be the two-level deterministic digest"
        );
        assert_ne!(id0, id1, "permanent and changing identifiers must differ");
    }

    #[test]
    fn deterministic_id_ignores_non_16_byte_source_id() {
        // A source /ID[0] that is not 16 bytes is not a usable permanent
        // identifier (it would break the fixed-width /ID array), so qpdf reuses
        // the changing identifier for both elements.
        let src = build_det_id_source(
            &format!("/ID [<{}><{}>]", "aa".repeat(20), "bb".repeat(16)),
            &[],
        );
        let out = write_det_id(&src);
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(id0, id1, "non-16-byte source /ID[0] must not be preserved");
        assert_eq!(id1, expected_changing_id(&out, b"").to_vec());
    }

    #[test]
    fn deterministic_id_ignores_non_string_array_source_id() {
        // A /ID whose elements are not strings (here two integers) is not a
        // usable permanent identifier; source_permanent_id returns None and the
        // changing identifier is reused for both elements.
        let src = build_det_id_source("/ID [1 2]", &[]);
        let out = write_det_id(&src);
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(id0, id1, "non-string source /ID must not be preserved");
        assert_eq!(id1, expected_changing_id(&out, b"").to_vec());
    }

    #[test]
    fn deterministic_id_seed_reads_inline_info_dictionary() {
        // /Info given inline (a direct dictionary, not an indirect reference):
        // its string values must still feed the seed.
        let src = build_det_id_source("/Info << /Title (Inline) >>", &[]);
        let out = write_det_id(&src);
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b" Inline").to_vec(),
            "an inline /Info dictionary must contribute to the seed"
        );
    }

    #[test]
    fn deterministic_id_ignores_non_dictionary_info() {
        // /Info that does not resolve to a dictionary (here a string) yields an
        // empty seed suffix, identical to having no /Info.
        let src = build_det_id_source("/Info (not a dict)", &[]);
        let out = write_det_id(&src);
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b"").to_vec(),
            "a non-dictionary /Info contributes nothing to the seed"
        );
    }

    #[test]
    fn deterministic_id_info_seed_changes_id() {
        // Two files with identical structure but different /Info string values
        // must produce different /ID[1] because /Info feeds the seed.
        let a = write_det_id(&build_det_id_source(
            "/Info 3 0 R",
            &["3 0 obj\n<< /Title (Alpha) >>\nendobj\n"],
        ));
        let b = write_det_id(&build_det_id_source(
            "/Info 3 0 R",
            &["3 0 obj\n<< /Title (Bravo) >>\nendobj\n"],
        ));
        assert_ne!(
            trailer_id_pair(&a).1,
            trailer_id_pair(&b).1,
            "different /Info string values must change /ID[1]"
        );
        // And the seed is exactly det_data + " QPDF " + " Alpha".
        assert_eq!(
            trailer_id_pair(&a).1,
            expected_changing_id(&a, b" Alpha").to_vec(),
            "/Info /Title (Alpha) contributes \" Alpha\" to the seed"
        );
    }

    #[test]
    fn deterministic_id_info_seed_sorts_keys_skips_non_strings_and_unescapes() {
        // /Info with keys out of sorted order (/Title before /Author), a
        // non-string entry (/Count 7, skipped), and an escaped literal string
        // (Hello\)World -> "Hello)World" after unescaping). The seed appends, in
        // SORTED key order, " " + decoded value for each string entry:
        //   " Bob" (Author) then " Hello)World" (Title).
        let src = build_det_id_source(
            "/Info 3 0 R",
            &["3 0 obj\n<< /Title (Hello\\)World) /Author (Bob) /Count 7 >>\nendobj\n"],
        );
        let out = write_det_id(&src);
        let suffix = b" Bob Hello)World";
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, suffix).to_vec(),
            "seed must use sorted keys, skip non-strings, and unescape values"
        );
    }

    #[test]
    fn deterministic_id_resolves_indirect_info_and_values() {
        // /Info is an indirect reference, and the /Title value is ALSO an
        // indirect reference. Both must be resolved so the string contributes
        // to the seed (PDF allows any value to be indirect).
        let src = build_det_id_source(
            "/Info 3 0 R",
            &[
                "3 0 obj\n<< /Title 4 0 R >>\nendobj\n",
                "4 0 obj\n(Indirect)\nendobj\n",
            ],
        );
        let out = write_det_id(&src);
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b" Indirect").to_vec(),
            "indirect /Info and indirect string value must be resolved into the seed"
        );
    }

    #[test]
    fn deterministic_id_empty_info_has_no_seed_suffix() {
        // /Info present but with no string entries: the seed suffix is empty,
        // identical to having no /Info at all.
        let with_empty_info = write_det_id(&build_det_id_source(
            "/Info 3 0 R",
            &["3 0 obj\n<< /Count 7 >>\nendobj\n"],
        ));
        assert_eq!(
            trailer_id_pair(&with_empty_info).1,
            expected_changing_id(&with_empty_info, b"").to_vec(),
            "an /Info with no string entries contributes nothing to the seed"
        );
    }

    #[test]
    fn deterministic_id_truncates_seed_at_first_nul() {
        // qpdf hashes the seed via `encodeString(seed.c_str())`, which stops at
        // the first NUL (strlen). A /Title carrying a NUL (here a UTF-16BE
        // string: BOM FEFF, then NUL-bearing code units) must therefore
        // contribute only the bytes BEFORE its first NUL to /ID[1]. The hex
        // string <feff0041> decodes to [0xFE, 0xFF, 0x00, 0x41], so the /Info
        // suffix is b" \xfe\xff\x00A" and the seed is truncated just after
        // b" \xfe\xff".
        let out = write_det_id(&build_det_id_source(
            "/Info 3 0 R",
            &["3 0 obj\n<< /Title <feff0041> >>\nendobj\n"],
        ));
        // expected_changing_id truncates the (suffix) seed at its first NUL too,
        // so passing the FULL suffix asserts the writer applied the same cut.
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b" \xfe\xff\x00A").to_vec(),
            "/ID[1] must be md5 of the seed truncated at the first NUL"
        );
        // Self-sufficient discriminator: the truncated /ID[1] must DIFFER from
        // the digest of the full (untruncated) seed, proving the cut happened.
        use md5::Digest as _;
        let bracket = id_array_bracket_offset(&out);
        let det_data = md5::Md5::digest(&out[..=bracket]);
        let mut full_seed = Vec::new();
        for byte in det_data.iter() {
            full_seed.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        full_seed.extend_from_slice(b" QPDF ");
        full_seed.extend_from_slice(b" \xfe\xff\x00A");
        let untruncated: [u8; 16] = md5::Md5::digest(&full_seed).into();
        assert_ne!(
            trailer_id_pair(&out).1,
            untruncated.to_vec(),
            "hashing the full (untruncated) seed must NOT match — the NUL cut is load-bearing"
        );
    }

    #[test]
    fn compute_deterministic_id_ignores_seed_bytes_after_first_nul() {
        // Isolated proof of seed truncation: with the body digest pinned (same
        // `bytes` and `id_array_offset`), only the info_suffix varies. qpdf's
        // strlen cut means bytes from the first NUL onward are excluded from the
        // changing identifier. (An end-to-end /Info test cannot isolate this:
        // the post-NUL bytes also feed the full-output body digest, so they
        // would change /ID[1] through det_data regardless of truncation.)
        let bytes = b"anything[";
        let offset = bytes.len() - 1; // the `[`
        let a = compute_deterministic_id(bytes, offset, b" \xfe\xff\x00AAA", None);
        let b = compute_deterministic_id(bytes, offset, b" \xfe\xff\x00BBB", None);
        assert_eq!(
            a.1, b.1,
            "info_suffix bytes after the first NUL must not affect /ID[1]"
        );
        // A byte BEFORE the NUL still matters, confirming the cut is at the NUL
        // and not earlier/later.
        let c = compute_deterministic_id(bytes, offset, b" \xfd\xff\x00AAA", None);
        assert_ne!(
            a.1, c.1,
            "info_suffix bytes before the first NUL must affect /ID[1]"
        );
        // A suffix with no NUL is hashed in full (control case).
        let d = compute_deterministic_id(bytes, offset, b" \xfe\xff", None);
        assert_eq!(
            a.1, d.1,
            "truncating at the NUL must equal hashing the pre-NUL bytes alone"
        );
    }

    #[test]
    fn write_deterministic_id_inline_matches_placeholder_then_patch() {
        // Inline direct-write must equal the legacy placeholder-then-patch result
        // for an identical prefix: same digest range (inclusive of `[`), same id.
        let prefix =
            b"%PDF-1.7\n1 0 obj<</X 1>>endobj\ntrailer << /Size 4 /Root 1 0 R /ID ".to_vec();

        let mut inline = prefix.clone();
        write_deterministic_id_inline(&mut inline, b"", None);

        // Legacy path: place the all-zero placeholder, then compute over [..='[']
        // (id_array_offset is the offset of the placeholder's opening `[`).
        let id_array_offset = prefix.len();
        let mut legacy_buf = prefix.clone();
        write_deterministic_id_array(&mut legacy_buf, &[0u8; 16], &[0u8; 16]);
        let (id0, id1) = compute_deterministic_id(&legacy_buf, id_array_offset, b"", None);
        let mut expect = prefix.clone();
        write_deterministic_id_array(&mut expect, &id0, &id1);

        assert_eq!(
            inline, expect,
            "inline direct-write must equal placeholder+patch output"
        );
        // And it must NOT be the all-zero placeholder.
        let mut placeholder = prefix.clone();
        write_deterministic_id_array(&mut placeholder, &[0u8; 16], &[0u8; 16]);
        assert_ne!(
            inline, placeholder,
            "inline must write the real id, not the placeholder"
        );
    }

    #[test]
    fn write_deterministic_id_inline_preserves_source_permanent_id() {
        // With a source permanent identifier supplied, /ID[0] (permanent) must
        // equal that source id and /ID[1] (changing) is the two-level digest, so
        // the inline write matches the placeholder-then-patch bytes for the same
        // computed id. Mirrors the full-buffer equivalence of the prior test.
        let prefix =
            b"%PDF-1.7\n1 0 obj<</X 1>>endobj\ntrailer << /Size 4 /Root 1 0 R /ID ".to_vec();
        let src0 = [0xAAu8; 16];

        let id_array_offset = prefix.len();
        let mut legacy_buf = prefix.clone();
        write_deterministic_id_array(&mut legacy_buf, &[0u8; 16], &[0u8; 16]);
        let (id0, id1) = compute_deterministic_id(&legacy_buf, id_array_offset, b"", Some(src0));
        let mut expect = prefix.clone();
        write_deterministic_id_array(&mut expect, &src0, &id1);

        let mut inline = prefix.clone();
        write_deterministic_id_inline(&mut inline, b"", Some(src0));

        assert_eq!(
            inline, expect,
            "inline write with a source id must equal placeholder+patch output"
        );
        assert_eq!(
            id0, src0,
            "/ID[0] must be the supplied permanent identifier"
        );
        assert_ne!(
            id0, id1,
            "permanent and changing identifiers must differ in general"
        );
    }

    #[test]
    fn write_pdf_with_id_writer_none_matches_write_pdf() {
        // With `id_writer = None`, the serializer must be byte-identical to the
        // plain `write_pdf` even when `/ID` is present — covering the fallback
        // arm that production (always `Some`) never exercises.
        let mut dict = Dictionary::new();
        dict.insert("Size", Object::Integer(4));
        dict.insert("Root", Object::reference(ObjectRef::new(1, 0)));
        dict.insert(
            "ID",
            Object::Array(vec![
                Object::String(vec![0xAB; 16]),
                Object::String(vec![0xCD; 16]),
            ]),
        );

        let mut plain = Vec::new();
        dict.write_pdf(&mut plain);
        let mut via_none = Vec::new();
        dict.write_pdf_with_id_writer(&mut via_none, None);
        assert_eq!(
            via_none, plain,
            "write_pdf_with_id_writer(None) must equal write_pdf byte-for-byte"
        );
    }

    #[test]
    fn write_stream_to_buf_with_id_writer_none_matches_write_stream_to_buf() {
        // The xref-stream helper documents byte-identity with `write_stream_to_buf`
        // when `id_writer = None`; production only ever passes `Some`, so this pins
        // the documented `None` contract across both the dictionary and the payload
        // framing (the helper is otherwise unreachable with `None`).
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::Integer(3));
        dict.insert(
            "ID",
            Object::Array(vec![
                Object::String(vec![0xAB; 16]),
                Object::String(vec![0xCD; 16]),
            ]),
        );
        let stream = crate::Stream::new(dict, b"abc".to_vec());

        let mut expected = Vec::new();
        write_stream_to_buf(&mut expected, &stream, NewlineBeforeEndstream::Yes);
        let mut actual = Vec::new();
        write_stream_to_buf_with_id_writer(&mut actual, &stream, NewlineBeforeEndstream::Yes, None);
        assert_eq!(
            actual, expected,
            "write_stream_to_buf_with_id_writer(None) must equal write_stream_to_buf"
        );
    }

    #[test]
    fn write_pdf_with_id_writer_keys_on_name_not_byte_pattern() {
        // The direct-write path keys on the `/ID` dictionary *name*, not on a
        // byte pattern. A preserved entry whose key sorts before `/ID` (here
        // `/AA`) and whose string value embeds the literal `/ID ` token plus the
        // all-zero placeholder array must survive verbatim, while only the real
        // `/ID` value is produced by the closure. This is exactly the ambiguity
        // the dropped byte-search patch scheme had to guard against.
        let mut decoy_value = b"/ID ".to_vec();
        write_deterministic_id_array(&mut decoy_value, &[0u8; 16], &[0u8; 16]);
        let mut dict = Dictionary::new();
        dict.insert("AA", Object::String(decoy_value.clone()));
        dict.insert(
            "ID",
            Object::Array(vec![
                Object::String(vec![0u8; 16]),
                Object::String(vec![0u8; 16]),
            ]),
        );

        let sentinel: &[u8] = b"[<DIGEST-FROM-CLOSURE>]";
        let mut id_writer = |out: &mut Vec<u8>| out.extend_from_slice(sentinel);
        let mut out = Vec::new();
        dict.write_pdf_with_id_writer(&mut out, Some(&mut id_writer));

        // The decoy value is serialized as a literal string `(...)` containing
        // the `/ID ` token and the placeholder array — it must appear verbatim.
        let mut decoy_serialized = Vec::new();
        Object::String(decoy_value).write_pdf(&mut decoy_serialized);
        assert!(
            out.windows(decoy_serialized.len())
                .any(|w| w == decoy_serialized.as_slice()),
            "preserved decoy entry embedding /ID + placeholder must survive verbatim"
        );
        // The real /ID value is the closure output, not the stored array.
        let id_token_pos = out
            .windows(5)
            .position(|w| w == b" /ID ")
            .expect("real /ID key token must be present");
        assert_eq!(
            &out[id_token_pos + 5..id_token_pos + 5 + sentinel.len()],
            sentinel,
            "the real /ID value must be the closure output, not the stored array"
        );
        // The closure ran exactly once: the sentinel appears a single time.
        let sentinel_count = out
            .windows(sentinel.len())
            .filter(|w| *w == sentinel)
            .count();
        assert_eq!(sentinel_count, 1, "id_writer must run exactly once");
    }

    #[test]
    fn deterministic_id_preserves_decoy_trailer_key_through_full_rewrite() {
        // End-to-end regression for the decoy-collision bug: a preserved (unknown)
        // trailer key `/Probe` whose value serializes to the EXACT 70-byte /ID
        // placeholder `[<0x32><0x32>]`. The full-rewrite trailer keeps unknown
        // keys (`trailer = pdf.trailer().clone()`) and forces /ID last, so the
        // serialized output is `... /Probe [<0..0><0..0>] ... /ID [<0..0><0..0>]`.
        // The direct-write path must emit the genuine /ID's digest (keyed on the
        // `/ID` name) and leave /Probe untouched.
        let zeros = "00000000000000000000000000000000"; // 16 zero bytes in hex
        let src = build_det_id_source(&format!("/Probe [<{zeros}><{zeros}>]"), &[]);
        let out = write_det_id(&src);

        // The decoy /Probe must survive as the original all-zero 16-byte array.
        let reopened = crate::Pdf::open_mem(&out).expect("output must re-open");
        let probe = reopened
            .trailer()
            .get("Probe")
            .and_then(Object::as_array)
            .expect("/Probe must be preserved as an array");
        assert_eq!(probe.len(), 2, "/Probe array arity must be preserved");
        for element in probe {
            assert_eq!(
                element
                    .as_string()
                    .expect("/Probe element must be a string"),
                &[0u8; 16],
                "/Probe must NOT be mis-patched — it stays the all-zero placeholder"
            );
        }
        // The genuine /ID[1] must be the non-zero computed identifier.
        assert_ne!(
            trailer_id_pair(&out).1,
            vec![0u8; 16],
            "the real /ID must be direct-written, not left as the zero placeholder"
        );
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b"").to_vec(),
            "/ID[1] must be the two-level deterministic digest"
        );
    }

    #[test]
    fn classic_trailer_deterministic_id_preserves_decoy_anchor_literal() {
        // Crafted-decoy survival guard for the CLASSIC (non-qdf) xref-table
        // trailer, the counterpart to
        // `qdf_trailer_deterministic_id_preserves_decoy_anchor_literal`. A
        // preserved (unknown) trailer key `/Decoy` whose STRING value's bytes
        // literally contain the `/ID ` token followed by the exact 70-byte
        // all-zero placeholder array. The full-rewrite classic trailer keeps
        // unknown keys (`trailer = pdf.trailer().clone()`) and forces the real
        // `/ID` last, so `/Decoy` sorts before it: a byte-search patch anchored
        // on the first `/ID ` occurrence would clobber the decoy and leave the
        // real `/ID` zeroed. The direct-write path never emits a placeholder and
        // never byte-searches, so the decoy survives verbatim and the genuine
        // /ID is the computed digest.
        let zeros = "00000000000000000000000000000000"; // 16 zero bytes in hex
        let decoy_literal = format!("/ID [<{zeros}><{zeros}>]");
        let src = build_det_id_source(&format!("/Decoy ({decoy_literal})"), &[]);
        let out = write_det_id(&src);

        // Confirm we exercised the classic `trailer` (Table xref) arm, not the
        // xref-stream form — otherwise this would silently duplicate the
        // xref-stream decoy coverage instead of guarding the classic flat path.
        assert!(
            out.windows(b"trailer ".len()).any(|w| w == b"trailer "),
            "classic deterministic-id output must use a classic `trailer` (Table xref form)"
        );

        // (a) The crafted decoy bytes must survive VERBATIM in the output — a
        // non-vacuous check that the literal `/ID `+placeholder run is present.
        assert!(
            out.windows(decoy_literal.len())
                .any(|window| window == decoy_literal.as_bytes()),
            "the crafted /Decoy bytes (`/ID `+placeholder) must appear verbatim"
        );
        // And the reopened /Decoy value is exactly the original literal string.
        let reopened = crate::Pdf::open_mem(&out).expect("output must re-open");
        let decoy = reopened
            .trailer()
            .get("Decoy")
            .and_then(Object::as_string)
            .expect("/Decoy must be preserved as a string");
        assert_eq!(
            decoy,
            decoy_literal.as_bytes(),
            "/Decoy must NOT be mis-patched — its `/ID `+placeholder bytes stay verbatim"
        );

        // (b) The genuine forced-last /ID[1] must be the non-zero computed digest
        // (not the all-zero placeholder).
        assert_ne!(
            trailer_id_pair(&out).1,
            vec![0u8; 16],
            "the real /ID must be direct-written, not left as the zero placeholder"
        );
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b"").to_vec(),
            "classic /ID[1] must be the two-level deterministic digest"
        );
    }

    #[test]
    fn classic_trailer_deterministic_id_is_direct_written_no_placeholder() {
        // The classic (non-qdf) xref-table trailer must DIRECT-WRITE the real
        // deterministic /ID inline, never leaving the all-zero placeholder for a
        // later byte-search patch. A clean fixture (no decoy keys) is used so the
        // only 70-byte `[<0..><0..>]` run that could appear would be a leftover
        // placeholder.
        let src = build_det_id_source("/Info 3 0 R", &["3 0 obj\n<< /Title (Doc) >>\nendobj\n"]);
        let out = write_det_id(&src);

        // (a) No all-zero /ID placeholder survives anywhere in the output.
        let mut placeholder = Vec::new();
        write_deterministic_id_array(&mut placeholder, &[0u8; 16], &[0u8; 16]);
        assert_eq!(placeholder.len(), 70, "placeholder must be 70 bytes");
        assert!(
            out.windows(placeholder.len())
                .all(|window| window != placeholder.as_slice()),
            "the all-zero /ID placeholder must not appear — /ID is direct-written"
        );

        // (b) The /ID array is the real digest and is deterministic across runs.
        let out2 = write_det_id(&src);
        assert_eq!(
            out, out2,
            "classic-trailer deterministic-id output must be byte-stable"
        );
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(
            id1,
            expected_changing_id(&out, b" Doc").to_vec(),
            "/ID[1] must be the two-level deterministic digest, not a placeholder"
        );
        assert_eq!(id0, id1, "absent source /ID makes /ID[0] equal /ID[1]");
        assert_ne!(
            id1,
            vec![0u8; 16],
            "/ID[1] must not be the zero placeholder"
        );
    }

    /// `qdf: true` + `deterministic_id: true` writer options, sharing the
    /// classic-xref-table fixture set.
    fn write_qdf_det_id(fixture: &[u8]) -> Vec<u8> {
        let opts = WriteOptions {
            full_rewrite: true,
            deterministic_id: true,
            qdf: true,
            ..WriteOptions::default()
        };
        let mut pdf = crate::Pdf::open_mem(fixture).expect("fixture must open");
        let mut out = Vec::new();
        write_pdf_with_options(&mut pdf, &mut out, &opts).expect("qdf deterministic write");
        out
    }

    #[test]
    fn qdf_trailer_deterministic_id_is_direct_written_no_placeholder() {
        // The qdf classic-table trailer must DIRECT-WRITE the real deterministic
        // /ID inline (via `write_qdf_trailer`'s id_writer), never leaving the
        // all-zero placeholder for a later byte-search patch. A clean fixture (no
        // decoy keys) is used so the only 70-byte `[<0..><0..>]` run that could
        // appear would be a leftover placeholder.
        //
        // Output is byte-identical to the old placeholder-then-patch result for
        // the same computed id, so the placeholder-absent / digest assertions
        // below are a regression guard, not a red-first failure: they hold under
        // byte-identity. The behavioral guard that the path no longer depends on
        // a byte-search patch lives in
        // `qdf_trailer_deterministic_id_preserves_decoy_anchor_literal` (a value
        // embedding the `/ID ` token + placeholder, which a first-match patch
        // would clobber).
        let src = build_det_id_source("/Info 3 0 R", &["3 0 obj\n<< /Title (Doc) >>\nendobj\n"]);
        let out = write_qdf_det_id(&src);

        // qdf forces an uncompressed classic xref table — confirm we exercised
        // the Table-arm qdf branch (not the xref-stream form).
        assert!(
            out.windows(b"trailer ".len()).any(|w| w == b"trailer "),
            "qdf output must use a classic `trailer` (Table xref form)"
        );

        // (a) No all-zero /ID placeholder survives anywhere in the output.
        let mut placeholder = Vec::new();
        write_deterministic_id_array(&mut placeholder, &[0u8; 16], &[0u8; 16]);
        assert_eq!(placeholder.len(), 70, "placeholder must be 70 bytes");
        assert!(
            out.windows(placeholder.len())
                .all(|window| window != placeholder.as_slice()),
            "the all-zero /ID placeholder must not appear — /ID is direct-written"
        );

        // (b) The /ID array is the real digest and is deterministic across runs.
        let out2 = write_qdf_det_id(&src);
        assert_eq!(
            out, out2,
            "qdf-trailer deterministic-id output must be byte-stable"
        );
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(
            id1,
            expected_changing_id(&out, b" Doc").to_vec(),
            "qdf /ID[1] must be the two-level deterministic digest, not a placeholder"
        );
        assert_eq!(id0, id1, "absent source /ID makes /ID[0] equal /ID[1]");
        assert_ne!(
            id1,
            vec![0u8; 16],
            "/ID[1] must not be the zero placeholder"
        );
    }

    #[test]
    fn qdf_trailer_deterministic_id_preserves_decoy_anchor_literal() {
        // Forward regression guard for the direct-write path: a preserved
        // (unknown) trailer key `/Decoy` whose STRING value's bytes literally
        // contain the `/ID ` token followed by the exact 70-byte all-zero
        // placeholder. `/Decoy` sorts before the forced-last real `/ID`, so a
        // byte-search patch anchored on the first `/ID ` occurrence would clobber
        // the decoy and leave the real `/ID` zeroed. The direct-write path never
        // emits the placeholder and never byte-searches, so the decoy survives
        // verbatim and the genuine /ID is the computed digest.
        let zeros = "00000000000000000000000000000000"; // 16 zero bytes in hex
        let decoy_literal = format!("/ID [<{zeros}><{zeros}>]");
        let src = build_det_id_source(&format!("/Decoy ({decoy_literal})"), &[]);
        let out = write_qdf_det_id(&src);

        // The decoy /Decoy must survive as the original literal string, untouched.
        let reopened = crate::Pdf::open_mem(&out).expect("output must re-open");
        let decoy = reopened
            .trailer()
            .get("Decoy")
            .and_then(Object::as_string)
            .expect("/Decoy must be preserved as a string");
        assert_eq!(
            decoy,
            decoy_literal.as_bytes(),
            "/Decoy must NOT be mis-patched — its `/ID `+placeholder bytes stay verbatim"
        );
        // The genuine /ID[1] must be the non-zero computed identifier.
        assert_ne!(
            trailer_id_pair(&out).1,
            vec![0u8; 16],
            "the real /ID must be direct-written, not left as the zero placeholder"
        );
        assert_eq!(
            trailer_id_pair(&out).1,
            expected_changing_id(&out, b"").to_vec(),
            "qdf /ID[1] must be the two-level deterministic digest"
        );
    }

    #[test]
    fn qdf_trailer_without_deterministic_id_serializes_stored_id() {
        // `write_qdf_trailer`'s `None` arm: plain qdf without `deterministic_id`
        // serializes the dictionary's stored /ID value verbatim (no id_writer
        // closure runs). Use `static_id` so the stored /ID is deterministic:
        // /ID[0] is the source permanent id, /ID[1] is the qpdf static constant.
        let src = build_det_id_source(
            "/ID [<0102030405060708090a0b0c0d0e0f10><1112131415161718191a1b1c1d1e1f20>]",
            &[],
        );
        let opts = WriteOptions {
            full_rewrite: true,
            qdf: true,
            static_id: true,
            ..WriteOptions::default()
        };
        let write = |f: &[u8]| {
            let mut pdf = crate::Pdf::open_mem(f).expect("fixture must open");
            let mut out = Vec::new();
            write_pdf_with_options(&mut pdf, &mut out, &opts).expect("qdf write");
            out
        };
        let out = write(&src);
        assert_eq!(out, write(&src), "static-id qdf output must be byte-stable");

        assert!(
            out.windows(b"trailer <<".len()).any(|w| w == b"trailer <<"),
            "qdf output must use the multi-line `trailer <<` layout"
        );
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(
            id0,
            (1u8..=16).collect::<Vec<u8>>(),
            "stored /ID[0] (source permanent id) must be serialized verbatim"
        );
        assert_eq!(
            id1.as_slice(),
            &QPDF_STATIC_ID[..],
            "stored /ID[1] (qpdf static constant) must be serialized verbatim"
        );
    }

    #[test]
    fn deterministic_id_xref_stream_is_self_stable() {
        // xref-stream form: qpdf does not produce byte-parity here, but the
        // content-derived /ID must still be deterministic (self-stable) and the
        // /ID[1] must match the two-level reconstruction.
        let fixture = build_partition_fixture();
        let opts = WriteOptions {
            full_rewrite: true,
            deterministic_id: true,
            // Generate ObjStm batches so the writer emits cross-reference
            // stream form rather than a classic xref table.
            object_streams: ObjectStreamMode::Generate,
            ..WriteOptions::default()
        };
        let write = |f: &[u8]| {
            let mut pdf = crate::Pdf::open_mem(f).expect("fixture must open");
            let mut out = Vec::new();
            write_pdf_with_options(&mut pdf, &mut out, &opts).expect("write");
            out
        };
        let o1 = write(&fixture);
        let o2 = write(&fixture);
        assert_eq!(o1, o2, "xref-stream deterministic-id output must be stable");
        let (id0, id1) = trailer_id_pair(&o1);
        assert_eq!(
            id1,
            expected_changing_id(&o1, b"").to_vec(),
            "xref-stream /ID[1] must match the two-level reconstruction"
        );
        assert_eq!(id0, id1, "absent source /ID makes /ID[0] equal /ID[1]");
    }

    #[test]
    fn deterministic_id_and_static_id_are_mutually_exclusive() {
        let fixture = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem(&fixture).expect("fixture must open");
        let opts = WriteOptions {
            full_rewrite: true,
            deterministic_id: true,
            static_id: true,
            ..WriteOptions::default()
        };
        let err = write_pdf_with_options(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("mutually exclusive")),
            "got {err:?}"
        );
    }

    #[test]
    fn deterministic_id_rejected_with_encryption() {
        let fixture = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem(&fixture).expect("fixture must open");
        let opts = WriteOptions {
            full_rewrite: true,
            deterministic_id: true,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriteOptions::default()
        };
        let err = write_pdf_with_options(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m)
                if m == "the deterministic-id option is incompatible with encrypted output files"),
            "got {err:?}"
        );
    }

    #[test]
    fn deterministic_id_requires_full_rewrite() {
        let fixture = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem(&fixture).expect("fixture must open");
        // full_rewrite defaults to false → incremental path.
        let opts = WriteOptions {
            deterministic_id: true,
            ..WriteOptions::default()
        };
        let err = write_pdf_with_options(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("requires a full rewrite")),
            "got {err:?}"
        );
    }

    // --- partition_objstm_eligible (flpdf-9hc.5.9, Task 1) ------------------

    /// Build a minimal xref-table PDF with five resolvable indirects:
    ///   1 0  Catalog            (plain dict — eligible, but used for /Root)
    ///   2 0  Pages              (plain dict — eligible)
    ///   3 0  neutral plain dict (eligible)
    ///   4 0  stream object      (ineligible — Object::Stream)
    ///   5 1  plain dict, gen 1  (ineligible — generation != 0)
    fn build_partition_fixture() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        // (object_number, generation, offset)
        let mut entries: Vec<(u32, u16, usize)> = Vec::new();

        entries.push((1, 0, bytes.len()));
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        entries.push((2, 0, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

        entries.push((3, 0, bytes.len()));
        bytes.extend_from_slice(b"3 0 obj\n<< /Subtype /Marker /Value 42 >>\nendobj\n");

        entries.push((4, 0, bytes.len()));
        let stream_data = b"hello";
        bytes.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", stream_data.len()).as_bytes(),
        );
        bytes.extend_from_slice(stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        entries.push((5, 1, bytes.len()));
        bytes.extend_from_slice(b"5 1 obj\n<< /Subtype /OldGen /Value 7 >>\nendobj\n");

        let startxref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", entries.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for (_num, generation, offset) in &entries {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
                entries.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn partition_objstm_eligible_splits_packable_and_plain_in_order() {
        let bytes = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem(&bytes).expect("fixture must open");

        let plain_dict = ObjectRef::new(3, 0);
        let stream_ref = ObjectRef::new(4, 0);
        let gen1_ref = ObjectRef::new(5, 1);

        // Sanity-check the fixture is wired correctly before exercising the
        // helper: each object must resolve to the expected variant.
        assert!(
            matches!(
                pdf.resolve_borrowed(plain_dict).unwrap(),
                Object::Dictionary(_)
            ),
            "obj 3 must resolve as a plain dictionary"
        );
        assert!(
            matches!(pdf.resolve_borrowed(stream_ref).unwrap(), Object::Stream(_)),
            "obj 4 must resolve as a stream"
        );
        assert!(
            matches!(
                pdf.resolve_borrowed(gen1_ref).unwrap(),
                Object::Dictionary(_)
            ),
            "obj 5 (gen 1) must resolve as a plain dictionary"
        );

        // Interleave the input so the assertion proves order preservation,
        // not mere set membership.
        let touched = [stream_ref, plain_dict, gen1_ref];
        let (packable, plain) =
            partition_objstm_eligible(&mut pdf, &touched).expect("partition must succeed");

        assert_eq!(
            packable,
            vec![plain_dict],
            "only the generation-0 non-stream dict is ObjStm-packable"
        );
        assert_eq!(
            plain,
            vec![stream_ref, gen1_ref],
            "stream and gen!=0 objects stay plain, in original order"
        );
    }

    // --- allocate_incremental_objstm_container (flpdf-9hc.5.9, Task 2) -------

    #[test]
    fn allocate_incremental_objstm_container_clears_all_input_space() {
        // Max source key = 11, max touched num = 9, max deleted num = 15.
        // The container must sit strictly above every input number so it can
        // never collide with a delete_object free entry: 15 + 1 = 16.
        let source_offsets: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::from([
            (4, (0, XrefOffset::Offset(100))),
            (11, (0, XrefOffset::Offset(200))),
        ]);
        let touched = vec![ObjectRef::new(7, 0), ObjectRef::new(9, 0)];
        let deleted = vec![ObjectRef::new(15, 0), ObjectRef::new(3, 0)];

        let container =
            allocate_incremental_objstm_container(&source_offsets, &touched, &deleted, 0)
                .expect("allocation must succeed");

        assert_eq!(
            container,
            ObjectRef::new(16, 0),
            "container number must be max(source,touched,deleted) + 1"
        );
        assert_eq!(container.generation, 0, "container generation must be 0");
    }

    #[test]
    fn allocate_incremental_objstm_container_respects_declared_size() {
        // Source/touched/deleted are all tiny, but declared /Size = 30 means
        // numbers up to 29 are reserved; the container must be 30, i.e.
        // declared_size.saturating_sub(1) + 1.
        let source_offsets: BTreeMap<u32, (u16, XrefOffset)> =
            BTreeMap::from([(2, (0, XrefOffset::Offset(50)))]);
        let touched = vec![ObjectRef::new(1, 0)];
        let deleted = vec![ObjectRef::new(3, 0)];

        let container =
            allocate_incremental_objstm_container(&source_offsets, &touched, &deleted, 30)
                .expect("allocation must succeed");

        assert_eq!(
            container,
            ObjectRef::new(30, 0),
            "declared_size must dominate when it exceeds all input numbers"
        );
    }

    #[test]
    fn allocate_incremental_objstm_container_zero_inputs() {
        // Empty source/touched/deleted and declared_size = 0 pins the lower
        // boundary: every max() collapses to 0, declared_size.saturating_sub(1)
        // is 0, and base + 1 = 1 -> ObjectRef::new(1, 0).
        let source_offsets: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::new();
        let touched: Vec<ObjectRef> = Vec::new();
        let deleted: Vec<ObjectRef> = Vec::new();

        let container =
            allocate_incremental_objstm_container(&source_offsets, &touched, &deleted, 0)
                .expect("allocation must succeed");

        assert_eq!(
            container,
            ObjectRef::new(1, 0),
            "zero/empty inputs must yield the minimal container number 1"
        );
    }

    #[test]
    fn merge_source_and_touched_offsets_for_xref_stream_handles_compressed() {
        // Source-only entry (must pass through unchanged).
        let mut source_offsets: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::new();
        source_offsets.insert(5, (0, XrefOffset::Offset(42)));

        // Plain touched entry -> XrefOffset::Offset.
        let mut touched: BTreeMap<u32, (u16, usize)> = BTreeMap::new();
        touched.insert(7, (0, 100));

        // Deleted ref -> XrefOffset::Free.
        let deleted = vec![ObjectRef::new(5, 0)];

        // Compressed (ObjStm member) entries -> XrefOffset::Compressed, gen 0.
        let mut compressed: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
        compressed.insert(9, (20, 3));

        let merged = merge_source_and_touched_offsets_for_xref_stream(
            &source_offsets,
            &touched,
            &deleted,
            &compressed,
        );

        // (i) compressed numbers become Compressed { stream, index } with gen 0.
        assert_eq!(
            merged.get(&9),
            Some(&(
                0,
                XrefOffset::Compressed {
                    stream: 20,
                    index: 3
                }
            )),
            "compressed member must become a type-2 Compressed entry with generation 0"
        );

        // (ii) plain touched entry still becomes Offset.
        assert_eq!(
            merged.get(&7),
            Some(&(0, XrefOffset::Offset(100))),
            "plain touched entry must still become an Offset entry"
        );

        // (iii) deleted ref still becomes Free.
        match merged.get(&5) {
            Some((_, XrefOffset::Free { .. })) => {}
            other => panic!("deleted ref must become a Free entry, got {other:?}"),
        }

        // (iv) source-only entry passes through unchanged when not touched/deleted.
        let mut source_only: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::new();
        source_only.insert(11, (2, XrefOffset::Offset(999)));
        let passthrough = merge_source_and_touched_offsets_for_xref_stream(
            &source_only,
            &BTreeMap::new(),
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(
            passthrough.get(&11),
            Some(&(2, XrefOffset::Offset(999))),
            "untouched source-only entry must pass through unchanged"
        );
    }

    #[test]
    fn merge_source_and_touched_offsets_for_xref_stream_overlap_precedence() {
        // Characterises the loop-ordering contract: touched -> compressed -> deleted.
        // Object 8: present in BOTH touched_offsets AND compressed_members.
        //           The compressed loop runs after the touched loop, so it must win.
        // Object 9: present in BOTH compressed_members AND deleted_object_refs.
        //           The deleted loop runs last, so Free must win.
        let source_offsets: BTreeMap<u32, (u16, XrefOffset)> = BTreeMap::new();

        let mut touched: BTreeMap<u32, (u16, usize)> = BTreeMap::new();
        touched.insert(8, (0, 100));

        let mut compressed_members: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
        compressed_members.insert(8, (20, 3));
        compressed_members.insert(9, (21, 4));

        let deleted = vec![ObjectRef::new(9, 0)];

        let merged = merge_source_and_touched_offsets_for_xref_stream(
            &source_offsets,
            &touched,
            &deleted,
            &compressed_members,
        );

        // Object 8: compressed loop runs after touched -> Compressed wins.
        assert_eq!(
            merged.get(&8),
            Some(&(
                0,
                XrefOffset::Compressed {
                    stream: 20,
                    index: 3
                }
            )),
            "object in both touched and compressed must resolve to Compressed (compressed loop runs after touched)"
        );

        // Object 9: deleted loop runs last -> Free wins over Compressed.
        match merged.get(&9) {
            Some((_, XrefOffset::Free { .. })) => {}
            other => panic!(
                "object in both compressed and deleted must resolve to Free (deleted loop runs last), got {other:?}"
            ),
        }
    }
    // --- write_incremental_objstm (flpdf-9hc.5.9, Task 4) -------------------

    /// Append a W=[1 3 1] xref-stream entry: 1-byte type, 3-byte big-endian
    /// field-1, 1-byte field-2.
    fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, f1: u32, f2: u8) {
        entries.push(entry_type);
        entries.push((f1 >> 16) as u8);
        entries.push((f1 >> 8) as u8);
        entries.push(f1 as u8);
        entries.push(f2);
    }

    /// Build a minimal xref-STREAM PDF (PDF-1.5) with three plain, generation-0,
    /// non-stream indirect objects resolvable through the xref stream:
    ///   1 0  Catalog            (plain dict — ObjStm-eligible)
    ///   2 0  Pages              (plain dict — ObjStm-eligible)
    ///   3 0  neutral plain dict (plain dict — ObjStm-eligible)
    ///   4 0  XRef stream        (self-referential, W=[1 3 1])
    fn build_xref_stream_fixture() -> Vec<u8> {
        let mut bytes = b"%PDF-1.5\n".to_vec();
        // (object_number, offset)
        let mut offsets: Vec<(u32, usize)> = Vec::new();

        offsets.push((1, bytes.len()));
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        offsets.push((2, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

        offsets.push((3, bytes.len()));
        bytes.extend_from_slice(b"3 0 obj\n<< /Subtype /Marker /Value 42 >>\nendobj\n");

        let xref_num: u32 = 4;
        let total_size = xref_num + 1; // entries 0..=4
        let xref_offset = bytes.len();

        // Xref stream payload (W=[1 3 1], /Index [0 total_size]).
        let mut entries: Vec<u8> = Vec::new();
        append_xref_stream_entry(&mut entries, 0, 0, 0); // 0: free head
        for (_num, off) in &offsets {
            append_xref_stream_entry(&mut entries, 1, *off as u32, 0);
        }
        // Object 4: the XRef stream itself (self-referential offset).
        append_xref_stream_entry(&mut entries, 1, xref_offset as u32, 0);

        bytes.extend_from_slice(
            format!(
                "{xref_num} 0 obj\n<< /Type /XRef /Size {total_size} /Root 1 0 R /W [1 3 1] /Index [0 {total_size}] /Length {} >>\nstream\n",
                entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    #[test]
    fn write_incremental_objstm_emits_container_and_member_index_map() {
        let fixture = build_xref_stream_fixture();
        let mut pdf = crate::Pdf::open_mem(&fixture).expect("xref-stream fixture must open");

        // Two plain, gen-0, ObjStm-eligible objects, resolved via the xref
        // stream. Sanity-check the fixture before exercising the emitter.
        let m0 = ObjectRef::new(2, 0); // Pages
        let m1 = ObjectRef::new(3, 0); // neutral plain dict
        assert!(
            matches!(pdf.resolve_borrowed(m0).unwrap(), Object::Dictionary(_)),
            "obj 2 must resolve from the xref stream as a plain dictionary"
        );
        assert!(
            matches!(pdf.resolve_borrowed(m1).unwrap(), Object::Dictionary(_)),
            "obj 3 must resolve from the xref stream as a plain dictionary"
        );

        let packable = [m0, m1];
        // Container number sits above every existing object (max 4) -> 5.
        let container_ref = ObjectRef::new(5, 0);
        let options = WriteOptions::default();

        let mut bytes: Vec<u8> = Vec::new();
        // Prefix bytes so the returned offset is provably relative to the
        // buffer start, not zero by accident.
        bytes.extend_from_slice(b"%incremental-tail-marker\n");
        let prefix_len = bytes.len();

        let (container_offset, members) =
            write_incremental_objstm(&mut bytes, &mut pdf, container_ref, &packable, &options)
                .expect("write_incremental_objstm must succeed");

        // Offset points at the appended container, after the prefix.
        assert_eq!(
            container_offset, prefix_len,
            "container_offset must mark where the container object begins"
        );

        // Member map is the packable slice paired with its 0-based index.
        assert_eq!(
            members,
            vec![(m0, 0u32), (m1, 1u32)],
            "members must be [(packable[0],0),(packable[1],1)] in packable order"
        );

        // The appended bytes carry a real serialised ObjStm container object.
        let container_bytes = &bytes[container_offset..];
        let container_str = String::from_utf8_lossy(container_bytes);
        assert!(
            container_str.starts_with(&format!("{} 0 obj", container_ref.number)),
            "container must open with `{} 0 obj`, got: {:?}",
            container_ref.number,
            &container_str[..container_str.len().min(32)]
        );
        assert!(
            container_str.contains("/Type /ObjStm"),
            "container dict must declare /Type /ObjStm"
        );
        assert!(
            container_str.contains("/N 2"),
            "container dict must declare /N 2 for the two packed members"
        );
        assert!(
            container_str.contains("endobj"),
            "container object must be terminated with endobj"
        );
    }

    // ── static_aes_iv tests (flpdf-9hc.4.13) ─────────────────────────────────

    /// Build an encrypted PDF with `static_aes_iv = true` and verify that
    /// every AES IV (the first 16 bytes of each ciphertext block) is all-zero.
    ///
    /// We also set `static_id = true` so that the file key — derived from
    /// `/ID[0]` — is deterministic; without that the file key itself changes
    /// between runs and the stream IV bytes would vary regardless.
    #[test]
    fn static_aes_iv_forces_all_zero_ivs_for_streams_and_strings() {
        use std::io::Cursor;

        // build_string_and_stream_fixture has a content stream reachable from
        // /Root (obj 4 = b"hello", referenced via the Catalog's /Metadata), so
        // it survives the Catalog-first reachability walk and encryption
        // exercises AES stream IV generation. minimal.pdf has no streams and no
        // encryptable strings, so it would never emit an IV and the zero-IV
        // assertion below would be vacuous.
        let fixture = build_string_and_stream_fixture();

        let mut pdf = Pdf::open(Cursor::new(fixture.clone())).expect("open fixture");
        let mut out = Vec::new();
        let options = WriteOptions {
            full_rewrite: true,
            static_id: true,
            static_aes_iv: true,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriteOptions::default()
        };
        write_pdf_with_options(&mut pdf, &mut out, &options).expect("encrypted write");

        // The output must be deterministic: running again with the same options
        // must produce byte-identical bytes.
        let mut pdf2 = Pdf::open(Cursor::new(fixture.clone())).unwrap();
        let mut out2 = Vec::new();
        write_pdf_with_options(&mut pdf2, &mut out2, &options).expect("encrypted write 2");
        assert_eq!(
            out, out2,
            "static_id + static_aes_iv must produce byte-identical output on two runs"
        );

        // Substantive check (the property the test name claims): each AES-CBC
        // stream stores its 16-byte IV as the first bytes of the stream payload
        // (PDF 1.7 §7.6.2). The full-rewrite writer serialises every stream as
        // `>>\nstream\n<payload>` (see `Object` serialisation), so the bytes
        // immediately following a `\nstream\n` delimiter are the IV. The
        // `\nendstream\n` terminator cannot alias this needle — the byte before
        // `stream` there is `d`, not `\n`. The encrypted path forces a classic
        // xref *table* and disables ObjStm, so every stream in the output is an
        // AES-encrypted content/metadata stream whose IV must be all-zero.
        const NEEDLE: &[u8] = b"\nstream\n";
        let mut checked = 0usize;
        let mut pos = 0usize;
        while let Some(rel) = out[pos..].windows(NEEDLE.len()).position(|w| w == NEEDLE) {
            let payload = pos + rel + NEEDLE.len();
            let iv = &out[payload..payload + 16];
            assert_eq!(
                iv,
                &[0u8; 16],
                "static_aes_iv: stream payload at byte {payload} must begin with a zero AES IV, got {iv:02x?}"
            );
            checked += 1;
            pos = payload;
        }
        assert!(
            checked > 0,
            "expected at least one encrypted stream to verify the static IV against"
        );
    }

    /// Without `static_aes_iv`, two encryptions of the same file produce
    /// different bytes because `/ID[1]` (and AES IVs on any content) are
    /// freshly random each run.  We use `static_id = false` so the trailer
    /// `/ID` already differs; the assertion captures the random-IV property
    /// at the level that is observable from the outside.
    #[test]
    fn without_static_aes_iv_two_runs_differ() {
        use std::io::Cursor;

        let input = include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec();

        let encrypt_once = || {
            let mut pdf = Pdf::open(Cursor::new(input.clone())).unwrap();
            let mut out = Vec::new();
            // static_id = false (default): /ID[1] is random → output differs
            let options = WriteOptions {
                full_rewrite: true,
                encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                    b"user".to_vec(),
                    b"owner".to_vec(),
                )),
                ..WriteOptions::default()
            };
            write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();
            out
        };

        let out1 = encrypt_once();
        let out2 = encrypt_once();
        assert_ne!(
            out1, out2,
            "without static_aes_iv + static_id the two encrypted outputs must differ (random /ID[1])"
        );
    }

    /// A minimal PDF carrying BOTH an `Object::String` (obj 3 `/Title`) and a
    /// content stream (obj 4 `hello`), so an encrypt round-trip exercises the
    /// string AND stream encryption passes — not just one. `/Info` references
    /// obj 3 so it is a live object.
    fn build_string_and_stream_fixture() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut entries: Vec<(u32, u16, usize)> = Vec::new();

        // The Catalog references the stream via /Metadata so it stays reachable
        // from /Root; the /Title dict is reachable as the trailer's /Info. Both
        // survive the writer's Catalog-first reachability walk (flpdf-9hc.32),
        // which drops objects unreachable from /Root and the trailer seeds.
        entries.push((1, 0, bytes.len()));
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 4 0 R >>\nendobj\n",
        );

        entries.push((2, 0, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

        entries.push((3, 0, bytes.len()));
        bytes.extend_from_slice(b"3 0 obj\n<< /Title (TopSecretTitle) >>\nendobj\n");

        entries.push((4, 0, bytes.len()));
        let stream_data = b"hello";
        bytes.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", stream_data.len()).as_bytes(),
        );
        bytes.extend_from_slice(stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let startxref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", entries.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for (_num, generation, offset) in &entries {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 3 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
                entries.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    /// Resolve the `/Title` string and the content-stream payload from a
    /// re-opened encrypted output of [`build_string_and_stream_fixture`].
    ///
    /// The Catalog-first renumber reassigns output object
    /// numbers, so navigate by reference (trailer `/Info` for the `/Title`
    /// dict, Catalog `/Metadata` for the stream) rather than hardcoding numbers.
    fn resolve_title_and_stream<R: Read + Seek>(rt: &mut Pdf<R>) -> (Vec<u8>, Vec<u8>) {
        let info_ref = match rt.trailer().get("Info") {
            Some(Object::Reference(r)) => *r,
            other => panic!("trailer /Info must be a reference, got {other:?}"),
        };
        let title = match rt.resolve(info_ref).expect("resolve /Info") {
            Object::Dictionary(d) => match d.get("Title") {
                Some(Object::String(s)) => s.clone(),
                other => panic!("/Title must be a string, got {other:?}"),
            },
            other => panic!("/Info must be a dictionary, got {other:?}"),
        };

        let root_ref = rt.root_ref().expect("root_ref");
        let metadata_ref = match rt.resolve(root_ref).expect("resolve /Root") {
            Object::Dictionary(d) => match d.get("Metadata") {
                Some(Object::Reference(r)) => *r,
                other => panic!("Catalog /Metadata must be a reference, got {other:?}"),
            },
            other => panic!("/Root must be a dictionary, got {other:?}"),
        };
        let stream = match rt.resolve(metadata_ref).expect("resolve /Metadata") {
            Object::Stream(s) => s.data,
            other => panic!("/Metadata must be a stream, got {other:?}"),
        };
        (title, stream)
    }

    /// Encrypt with V=5 R=6 AES-256, then re-open with flpdf
    /// using EACH password and confirm the string AND stream decrypt back to
    /// their original plaintext. This exercises the V=5 file-key-direct
    /// AES-256 string pass and stream pass via the reader. V=5 has random
    /// salts + FEK, so there is no byte-identical determinism to assert — this
    /// password round-trip is the correctness gate.
    #[test]
    fn v5_r6_encrypt_round_trips_string_and_stream_via_reader() {
        use crate::PdfOpenOptions;
        use std::io::Cursor;

        let fixture = build_string_and_stream_fixture();
        let mut pdf = Pdf::open(Cursor::new(fixture.clone())).expect("open fixture");
        let mut out = Vec::new();
        let options = WriteOptions {
            full_rewrite: true,
            // Keep the stream uncompressed so the decrypted payload is exactly
            // the original bytes (no filter round-trip to account for).
            compress_streams: CompressStreams::No,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v5_r6(
                b"user-pw".to_vec(),
                b"owner-pw".to_vec(),
            )),
            ..WriteOptions::default()
        };
        write_pdf_with_options(&mut pdf, &mut out, &options).expect("V=5 R=6 encrypted write");

        for pw in [b"user-pw".as_slice(), b"owner-pw".as_slice()] {
            let label = String::from_utf8_lossy(pw).into_owned();
            let mut rt = Pdf::open_with_options(
                Cursor::new(out.clone()),
                PdfOpenOptions {
                    password: pw.to_vec(),
                    ..PdfOpenOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("re-open of V=5 output with {label:?} failed: {e}"));

            // String pass: /Title (via trailer /Info) must decrypt to plaintext.
            // Stream pass: /Metadata (via Catalog) payload must decrypt.
            let (title, stream) = resolve_title_and_stream(&mut rt);
            assert_eq!(
                title.as_slice(),
                b"TopSecretTitle",
                "V=5 R=6 string must round-trip via reader for {label:?}"
            );
            assert_eq!(
                stream.as_slice(),
                b"hello",
                "V=5 R=6 stream must round-trip via reader for {label:?}"
            );
        }
    }

    /// Encrypt with V=5 R=5 (--force-R5), then re-open with flpdf
    /// using the user password and verify strings and streams round-trip.
    #[test]
    fn v5_r5_encrypt_round_trips_string_and_stream_via_reader() {
        use crate::PdfOpenOptions;
        use std::io::Cursor;

        let fixture = build_string_and_stream_fixture();
        let mut pdf = Pdf::open(Cursor::new(fixture.clone())).expect("open fixture");
        let mut out = Vec::new();
        let options = WriteOptions {
            full_rewrite: true,
            // Keep the stream uncompressed so the decrypted payload is exactly
            // the original bytes (no filter round-trip to account for).
            compress_streams: CompressStreams::No,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v5_r5(
                b"user-pw".to_vec(),
                b"owner-pw".to_vec(),
            )),
            ..WriteOptions::default()
        };
        write_pdf_with_options(&mut pdf, &mut out, &options).expect("V=5 R=5 encrypted write");

        for pw in [b"user-pw".as_slice(), b"owner-pw".as_slice()] {
            let label = String::from_utf8_lossy(pw).into_owned();
            let mut rt = Pdf::open_with_options(
                Cursor::new(out.clone()),
                PdfOpenOptions {
                    password: pw.to_vec(),
                    // R=5 is flagged as weak crypto by the reader (deprecated
                    // pre-ISO revision); allow it explicitly in this writer test.
                    allow_weak_crypto: true,
                    ..PdfOpenOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("re-open of V=5 R=5 output with {label:?} failed: {e}"));

            // String pass: /Title (via trailer /Info) must decrypt to plaintext.
            // Stream pass: /Metadata (via Catalog) payload must decrypt.
            let (title, stream) = resolve_title_and_stream(&mut rt);
            assert_eq!(
                title.as_slice(),
                b"TopSecretTitle",
                "V=5 R=5 string must round-trip via reader for {label:?}"
            );
            assert_eq!(
                stream.as_slice(),
                b"hello",
                "V=5 R=5 stream must round-trip via reader for {label:?}"
            );
        }
    }

    /// Each RC4 writer method (V=1 RC4-40, V=2 RC4-128,
    /// V=4 RC4-128) round-trips. Encrypt a string+stream fixture, then re-open
    /// with flpdf under EACH password and confirm `/Title` and the stream
    /// decrypt to plaintext. The reader gates RC4 behind weak-crypto, so the
    /// re-open sets `allow_weak_crypto = true`.
    #[test]
    fn rc4_methods_round_trip_string_and_stream_via_reader() {
        use crate::encrypt_setup::{EncryptMethod, EncryptParams};
        use crate::PdfOpenOptions;
        use std::io::Cursor;

        let fixture = build_string_and_stream_fixture();
        for method in [
            EncryptMethod::V1Rc440,
            EncryptMethod::V2Rc4128,
            EncryptMethod::V4Rc4128,
        ] {
            let mut pdf = Pdf::open(Cursor::new(fixture.clone())).expect("open fixture");
            let mut out = Vec::new();
            let options = WriteOptions {
                full_rewrite: true,
                // Keep the stream uncompressed so the decrypted payload equals
                // the original bytes.
                compress_streams: CompressStreams::No,
                encrypt: Some(EncryptParams::rc4(
                    method,
                    b"user-pw".to_vec(),
                    b"owner-pw".to_vec(),
                )),
                ..WriteOptions::default()
            };
            write_pdf_with_options(&mut pdf, &mut out, &options)
                .unwrap_or_else(|e| panic!("{method:?} encrypted write failed: {e}"));

            for pw in [b"user-pw".as_slice(), b"owner-pw".as_slice()] {
                let label = format!("{method:?}/{}", String::from_utf8_lossy(pw));
                let mut rt = Pdf::open_with_options(
                    Cursor::new(out.clone()),
                    PdfOpenOptions {
                        password: pw.to_vec(),
                        allow_weak_crypto: true,
                        ..PdfOpenOptions::default()
                    },
                )
                .unwrap_or_else(|e| panic!("re-open {label} failed: {e}"));

                // Navigate by reference (trailer /Info, Catalog /Metadata)
                // rather than hardcoded numbers, since output is renumbered.
                let (title, stream) = resolve_title_and_stream(&mut rt);
                assert_eq!(
                    title.as_slice(),
                    b"TopSecretTitle",
                    "{label} string round-trip"
                );
                assert_eq!(stream.as_slice(), b"hello", "{label} stream round-trip");
            }
        }
    }

    /// V=4 encryption (/V 4, AES-128 or RC4-128) requires a PDF header >= 1.5.
    /// The encrypted path uses a classic xref table, so the writer must floor
    /// the header explicitly — a 1.4 input must not emit a 1.4 header with /V 4.
    #[test]
    fn v4_encryption_floors_pdf_header_to_1_5() {
        use crate::encrypt_setup::{EncryptMethod, EncryptParams};
        use std::io::Cursor;

        let fixture = build_partition_fixture();
        assert!(
            fixture.starts_with(b"%PDF-1.4"),
            "fixture must start at %PDF-1.4 for this test to be meaningful"
        );

        for params in [
            EncryptParams::v4_aes128(b"u".to_vec(), b"o".to_vec()),
            EncryptParams::rc4(EncryptMethod::V4Rc4128, b"u".to_vec(), b"o".to_vec()),
        ] {
            let mut pdf = Pdf::open(Cursor::new(fixture.clone())).unwrap();
            let mut out = Vec::new();
            let options = WriteOptions {
                full_rewrite: true,
                encrypt: Some(params),
                ..WriteOptions::default()
            };
            write_pdf_with_options(&mut pdf, &mut out, &options).expect("V=4 encrypted write");
            assert!(
                out.starts_with(b"%PDF-1.5"),
                "V=4 encryption must floor the header to 1.5, got {:?}",
                String::from_utf8_lossy(&out[..out.len().min(12)])
            );
        }
    }

    /// A minimal PDF whose `/Catalog` references a `/Metadata` XMP stream
    /// (obj 4), carrying a recognizable marker.
    fn build_metadata_fixture() -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut entries: Vec<(u32, u16, usize)> = Vec::new();

        entries.push((1, 0, bytes.len()));
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 4 0 R >>\nendobj\n",
        );
        entries.push((2, 0, bytes.len()));
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        entries.push((3, 0, bytes.len()));
        bytes.extend_from_slice(b"3 0 obj\n<< /Subtype /Marker /Value 1 >>\nendobj\n");

        entries.push((4, 0, bytes.len()));
        let xmp: &[u8] =
            b"<?xpacket?><x:xmpmeta>SECRET-XMP-MARKER</x:xmpmeta><?xpacket end=\"w\"?>";
        bytes.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n",
                xmp.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(xmp);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let startxref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", entries.len() + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for (_num, generation, offset) in &entries {
            bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
                entries.len() + 1
            )
            .as_bytes(),
        );
        bytes
    }

    /// With `encrypt_metadata = false` the `/Catalog`'s
    /// `/Metadata` stream is left UNENCRYPTED (its bytes survive in the clear)
    /// and tagged `/Crypt`, and the `/Encrypt` dict carries `/EncryptMetadata
    /// false` — whereas the default (`encrypt_metadata = true`) ciphers it.
    #[test]
    fn cleartext_metadata_exempts_metadata_stream_from_encryption() {
        use std::io::Cursor;

        const MARKER: &[u8] = b"SECRET-XMP-MARKER";
        let fixture = build_metadata_fixture();
        let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);

        let encrypt = |encrypt_metadata: bool| -> Vec<u8> {
            let mut pdf = Pdf::open(Cursor::new(fixture.clone())).unwrap();
            let mut out = Vec::new();
            let mut params =
                crate::encrypt_setup::EncryptParams::v4_aes128(b"u".to_vec(), b"o".to_vec());
            params.encrypt_metadata = encrypt_metadata;
            let options = WriteOptions {
                full_rewrite: true,
                // No compression so cleartext metadata is a literal substring.
                compress_streams: CompressStreams::No,
                encrypt: Some(params),
                ..WriteOptions::default()
            };
            write_pdf_with_options(&mut pdf, &mut out, &options).expect("encrypted write");
            out
        };

        // Default: /Metadata is encrypted, so the marker does not appear.
        let enc = encrypt(true);
        assert!(
            !contains(&enc, MARKER),
            "default encrypt must cipher the /Metadata stream"
        );

        // --cleartext-metadata: the XMP marker survives in the clear, the dict
        // emits /EncryptMetadata false, and the stream is tagged /Crypt.
        let ct = encrypt(false);
        assert!(
            contains(&ct, MARKER),
            "cleartext metadata must leave the XMP stream unencrypted"
        );
        assert!(
            contains(&ct, b"/EncryptMetadata false"),
            "the /Encrypt dict must carry /EncryptMetadata false"
        );
        assert!(
            contains(&ct, b"/Crypt"),
            "the exempt /Metadata stream must be tagged with a /Crypt filter"
        );
    }
}
