#[path = "writer/object_streams.rs"]
pub(crate) mod object_streams;
pub use object_streams::ObjectStreamMode;

use crate::parser::Parser;
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

/// Controls whether a newline is explicitly inserted immediately before the
/// `endstream` keyword.
///
/// ISO 32000-1 §7.3.8.1 requires an end-of-line marker before `endstream`.
/// qpdf exposes this as `--newline-before-endstream=y/n`.  The default is
/// [`NewlineBeforeEndstream::Yes`], matching qpdf's default behaviour.
///
/// # Byte-level semantics
///
/// When `Yes`: exactly one `b'\n'` is written immediately before `endstream`,
/// regardless of whether the stream payload already ends with a newline.  The
/// `/Length` dictionary entry is **not** modified — it continues to reflect the
/// raw payload length only, not the extra newline byte (matching qpdf parity).
///
/// When `No`: no newline is written before `endstream` unless the payload does
/// not already end with a newline character (`\n` or `\r`), in which case a
/// single `b'\n'` is appended to maintain ISO 32000-1 parseability.  The
/// `/Length` value is likewise unmodified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewlineBeforeEndstream {
    /// Explicitly write exactly one `b'\n'` before `endstream` (default).
    ///
    /// This guarantees the invariant required by ISO 32000-1 §7.3.8.1 and
    /// matches qpdf's `--newline-before-endstream=y` behaviour.
    #[default]
    Yes,
    /// Do not add an extra newline before `endstream`.
    ///
    /// If the stream payload already ends with `\n` or `\r`, `endstream` is
    /// written immediately after the payload (adjacency).  If the payload does
    /// not end with a newline, a single `b'\n'` is inserted to preserve
    /// parseability — matching qpdf's `--newline-before-endstream=n` behaviour.
    No,
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
    /// `--no-original-object-ids` behaviour exactly (implemented in epic layer
    /// `flpdf-9hc.6.5`).
    pub no_original_object_ids: bool,

    /// When `true`, decode every stream through its filter pipeline and re-emit
    /// the document end-to-end with a single `/FlateDecode` filter applied to
    /// each stream.  The output contains no `/Prev` chain, no `ObjStm`, and no
    /// xref-stream-only keys.
    ///
    /// When `false` (the default) the existing incremental-update write path is
    /// used, preserving the source bytes verbatim.
    pub full_rewrite: bool,

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
    /// governed solely by `compress_streams` and are not affected by this flag
    /// (QDF-specific xref/ObjStm behaviour is handled by later epic layers).
    ///
    /// The CLI flag `--qdf` is wired up in epic layer 6.8; until then this field
    /// is the library-level entry point.  Test via
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

    /// Encrypt the output with the supplied [`EncryptParams`] (qpdf
    /// `--encrypt …` equivalent — flpdf-9hc.4.9).
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
    ///    prepended + PKCS#7 padding, `/Length` updated to match), via
    ///    the helpers from flpdf-9hc.4.5 / 4.6.
    /// 4. Emits the `/Encrypt` dictionary itself as a plaintext indirect
    ///    object whose number is referenced from the trailer.
    ///
    /// **Required flag combinations** (the writer rejects others to keep
    /// the walking-skeleton scope tractable; subsequent PRs in
    /// flpdf-9hc.4.9 lift these as they wire each path):
    ///
    /// - `full_rewrite` is implicitly forced to `true` (the incremental
    ///   path cannot rewrite source object bytes).
    /// - `qdf` must be `false` (QDF mode emits plaintext for human
    ///   inspection; the combination is rejected with `Unsupported`).
    /// - `object_streams` is implicitly forced to
    ///   [`ObjectStreamMode::Disable`] (ObjStm containers encrypt as a
    ///   single blob, not per-member, requiring a separate code path).
    pub encrypt: Option<crate::encrypt_setup::EncryptParams>,

    /// Copy the `/Encrypt` dictionary verbatim from a donor PDF and re-use its
    /// file encryption key (qpdf `--copy-encryption-from` equivalent —
    /// flpdf-9hc.4.11).
    ///
    /// When set the writer bypasses the normal password-derivation path and
    /// constructs an [`EncryptionContext`] directly from the pre-recovered file
    /// key, the donor's `/Encrypt` dict, and the donor's `/ID[0]`.  The output
    /// can therefore be decrypted with the donor's user and owner passwords.
    ///
    /// Exactly one of `encrypt` and `copy_encryption` may be set; the CLI
    /// enforces mutual exclusion via `conflicts_with`.  The writer asserts this
    /// invariant at the top of the full-rewrite path.
    ///
    /// **Scope:** Only V=4 AES-128 donors are supported (flpdf-9hc.4.9 walking
    /// skeleton); other schemes are rejected by the CLI before this field is
    /// populated.
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
/// 3. If `linearize` is true, apply an additional `max(…, "1.2")` floor
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
const QPDF_BINARY_MARKER: &[u8] = b"%\xbf\xf7\xa2\xfe\n";

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
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, out: W) -> Result<()> {
    write_pdf_with_options(pdf, out, &WriteOptions::default())
}

/// Like [`write_pdf`] but with caller-supplied [`WriteOptions`].
pub fn write_pdf_with_options<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriteOptions,
) -> Result<()> {
    if options.full_rewrite {
        return write_pdf_full_rewrite(pdf, out, options);
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

    for object_ref in pdf.resolved_object_refs() {
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
/// Wired into the incremental Generate-mode gate by `write_pdf_incremental`
/// (flpdf-9hc.5.9 Task 5).
fn partition_objstm_eligible<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    touched: &[ObjectRef],
) -> Result<(Vec<ObjectRef>, Vec<ObjectRef>)> {
    let ctx = object_streams::eligibility_context(pdf)?;
    let mut packable = Vec::new();
    let mut plain = Vec::new();
    for &r in touched {
        let obj = pdf.resolve(r)?;
        if object_streams::is_eligible_for_objstm(r, &obj, &ctx) {
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
        let object = pdf.resolve(*object_ref)?;
        let Some(offset) = write_object(bytes, *object_ref, &object)? else {
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
/// Wired into the incremental write path by `write_pdf_incremental`
/// (flpdf-9hc.5.9 Task 5).
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
/// member map is consumed by `write_pdf_incremental` (flpdf-9hc.5.9 Task 5)
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
/// Wired into the incremental write path by `write_pdf_incremental`
/// (flpdf-9hc.5.9 Task 5).
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
/// uses. It therefore goes through the QDF serializers built by epic
/// `flpdf-9hc.6` (decoded streams, indirect `/Length` holders, `%QDF-1.0`
/// header, `%% Original object ID:` comments, classic `xref` table, and the
/// `trailer <<` dict layout). The previous standalone implementation used the
/// compact non-QDF serializers and diverged from this path (flpdf-9hc.24).
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
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
/// # Scope limitations (TODO)
///
/// - **ObjStm dissolve**: Object streams are dissolved — members are emitted as
///   ordinary indirect objects.  There is currently no merging of existing
///   ObjStm containers back into the regular sequence; they are simply skipped.
///   A dedicated "renumber + pack into ObjStm" pass (flpdf-9hc.20.13) is a
///   future concern.
///
/// - **Encrypted documents**: authenticated inputs are rewritten as plaintext
///   by default; pass [`WriteOptions::encrypt`] to produce encrypted output
///   instead (flpdf-9hc.4.9 walking skeleton — V=4 AES-128 only).
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
mod _writer_doc_anchor {} // keeps the `write_pdf_full_rewrite` docstring above attached to its function.

// ── Encryption context (flpdf-9hc.4.9) ───────────────────────────────────────

/// Per-write encryption state used by the full-rewrite path when
/// [`WriteOptions::encrypt`] is set. Built once at the top of
/// [`write_pdf_full_rewrite`] via [`build_encryption_context`] and consumed
/// by the per-object emission loop + the trailer-build step.
struct EncryptionContext {
    /// Built `/Encrypt` dictionary (from a 4.1/4.2/4.3 builder).
    encrypt_dict: Dictionary,
    /// File encryption key derived from passwords + `/ID[0]` (Algorithm 2).
    file_key: Vec<u8>,
    /// Per-object key derivation variant for Algorithm 1
    /// (`Aes` adds the `sAlT` salt; `Rc4` does not).
    object_key_alg: crate::security::standard::ObjectKeyAlg,
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
}

fn build_encryption_context<R: Read + Seek>(
    pdf: &Pdf<R>,
    options: &WriteOptions,
    params: &crate::encrypt_setup::EncryptParams,
    existing_max: u32,
) -> Result<EncryptionContext> {
    use crate::encrypt_setup::EncryptMethod;
    use crate::security::standard::{
        build_v4_encrypt_dict, ObjectKeyAlg, V4CryptMethod, V4EncryptParams,
    };

    // Resolve /ID[0] BEFORE deriving the file key. Algorithm 2 uses /ID[0]
    // as a salt; the trailer must carry the same bytes so the reader can
    // re-derive the file key from the password.
    let id0 = resolve_id0_for_encryption(pdf, options);

    let (encrypt_dict, file_key, object_key_alg) = match params.method {
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
            (dict, key, ObjectKeyAlg::Aes)
        }
    };

    // Allocate the /Encrypt object number above all existing objects. The
    // encrypted path forces object_streams=disable (so no container numbers
    // sit between existing_max and our allocation) and forbids QDF length
    // holders, so existing_max + 1 is a safe slot.
    let encrypt_num = existing_max.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported(
            "full-rewrite encrypt: /Encrypt object number overflows u32".to_string(),
        )
    })?;

    Ok(EncryptionContext {
        encrypt_dict,
        file_key,
        object_key_alg,
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
        id0,
        static_aes_iv: options.static_aes_iv,
    })
}

/// Build an [`EncryptionContext`] from a donor [`CopyEncryptionSource`]
/// (flpdf-9hc.4.11 `--copy-encryption-from` path).
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
        object_key_alg: src.object_key_alg,
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
        id0: src.id0.clone(),
        static_aes_iv: options.static_aes_iv,
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

/// Fill the trailer's `/Encrypt` and `/ID` entries appropriately for both
/// the plaintext and encrypted output paths.
fn apply_encrypt_trailer_entries<R: Read + Seek>(
    trailer: &mut Dictionary,
    pdf: &Pdf<R>,
    options: &WriteOptions,
    encrypt_ctx: Option<&EncryptionContext>,
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
        if options.static_id {
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

    let per_obj_key = per_object_key(
        &ctx.file_key,
        object_ref.number,
        u32::from(object_ref.generation),
        ctx.object_key_alg,
    );

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

    match ctx.object_key_alg {
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

    let per_obj_key = per_object_key(
        &ctx.file_key,
        object_ref.number,
        u32::from(object_ref.generation),
        ctx.object_key_alg,
    );

    let mut iv = [0u8; 16];
    if matches!(ctx.object_key_alg, ObjectKeyAlg::Aes) && !ctx.static_aes_iv {
        // Propagate OS-RNG failures (e.g. restricted WASM sandbox, exhausted
        // entropy in a chroot at boot) as `Unsupported` instead of panicking.
        // This site returns `Result`, so propagation is straightforward; the
        // string-encryption walker counterpart in
        // `encrypt_strings_in_object_for_writer` goes through a `FnMut`
        // closure that cannot today propagate the error — that path is
        // tracked separately (see flpdf-9hc.4.9 follow-up).
        getrandom::getrandom(&mut iv).map_err(|e| {
            crate::Error::Unsupported(format!(
                "OS CSPRNG (getrandom) unavailable for AES IV generation: {e}"
            ))
        })?;
    }

    match ctx.object_key_alg {
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

    // /Length reflects the encrypted on-disk byte count.
    let new_len = i64::try_from(stream.data.len()).map_err(|_| {
        crate::Error::Unsupported("encrypted stream /Length overflows i64".to_string())
    })?;
    stream.dict.insert("Length", Object::Integer(new_len));
    Ok(())
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

    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut version = effective_pdf_version(pdf.version(), options, false).to_owned();

    // ── flpdf-9hc.4.9 / 4.11 walking-skeleton encryption preflight ─────────
    // The encrypted-output path (both --encrypt and --copy-encryption-from)
    // piggybacks on the simple full-rewrite case: classic xref Table form, no
    // ObjStm batches, no QDF.  Reject incompatible flag combinations upfront
    // with a clear diagnostic rather than silently producing a corrupt file.
    //
    // Invariant: at most ONE of encrypt / copy_encryption is set.  The CLI
    // enforces this via conflicts_with; assert here for safety so library
    // callers are also caught.
    assert!(
        !(options.encrypt.is_some() && options.copy_encryption.is_some()),
        "encrypt and copy_encryption are mutually exclusive"
    );
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();

    if encrypting && options.qdf {
        return Err(crate::Error::Unsupported(
            "--encrypt / --copy-encryption-from cannot be combined with --qdf \
             (flpdf-9hc.4.9 walking skeleton)"
                .to_string(),
        ));
    }

    // ── Step 1: run the ObjStm planner ───────────────────────────────────────
    // For the encrypted path, force ObjStm off — ObjStm containers encrypt as
    // a single blob per PDF 1.7 §7.5.7, not per-member, which needs its own
    // code path (flpdf-9hc.4.9 follow-up). Override the caller's
    // object_streams setting with Disable when encryption is active.
    let planner_options;
    let planner_config = if encrypting {
        planner_options = WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            ..options.clone()
        };
        object_streams::planner_config_from_options(&planner_options)
    } else {
        object_streams::planner_config_from_options(options)
    };
    let plan = object_streams::plan_object_streams(pdf, &planner_config)?;

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

    // flpdf-9hc.4.9 / 4.11 walking skeleton: force classic xref Table form
    // for the encrypted path (both --encrypt and --copy-encryption-from).
    // Xref streams can carry /Encrypt in their dict, but the emission path
    // doesn't yet exclude the xref stream's own bytes from encryption —
    // needs its own handling. Table form is what qpdf emits for default
    // `--encrypt …` output so this matches user expectation.
    if encrypting {
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

    // ── Step 2 & 3: build member→batch lookup and allocate container numbers ─
    let mut object_refs = pdf.object_refs();
    object_refs.sort_by_key(|r| (r.number, r.generation));

    let existing_max: u32 = object_refs.iter().map(|r| r.number).max().unwrap_or(0);

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

    // member_to_batch: ObjectRef → (container_obj_num, index_in_batch)
    use std::collections::HashMap;
    let mut member_to_batch: HashMap<ObjectRef, (u32, u32)> = HashMap::new();
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_num = container_refs[batch_idx].number;
        for (idx_in_batch, &member_ref) in batch.iter().enumerate() {
            member_to_batch.insert(member_ref, (container_num, idx_in_batch as u32));
        }
    }

    // ── flpdf-9hc.4.9 / 4.11 walking-skeleton encryption context ───────────
    // Built ONCE up front (not inside the emission loop) so:
    // - /ID[0] is decided before any object is encrypted (Algorithm 2 derives
    //   the file key from /ID[0]; the trailer must carry the SAME bytes).
    // - The /Encrypt object's number is allocated above all existing objects
    //   and ObjStm containers — known here because we've forced
    //   object_streams=disable for the encrypted path (no containers exist).
    let encrypt_ctx: Option<EncryptionContext> = if let Some(ref params) = options.encrypt {
        Some(build_encryption_context(
            pdf,
            options,
            params,
            existing_max,
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
    let mut existing_holders: HashSet<u32> = HashSet::new();
    if options.qdf {
        for object_ref in &object_refs {
            if Some(*object_ref) == pdf.encryption_ref() || member_to_batch.contains_key(object_ref)
            {
                continue;
            }
            if let Ok(Object::Stream(s)) = pdf.resolve(*object_ref) {
                if let Some(Object::Reference(r)) = s.dict.get("Length") {
                    existing_holders.insert(r.number);
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

    // QDF framing (flpdf-9hc.6.10): qpdf `--qdf` never emits a body object for
    // object 0 (the xref free-list head) or for any free/deleted entry — those
    // exist only as `f` rows in the regenerated xref table. flpdf's
    // `object_refs()` includes the free head (0 65535) and any deleted refs, so
    // on the qdf path we suppress them here. The non-qdf path is unaffected and
    // its byte output is unchanged.
    let qdf_skip_refs: std::collections::HashSet<ObjectRef> = if options.qdf {
        pdf.deleted_object_refs().into_iter().collect()
    } else {
        std::collections::HashSet::new()
    };

    for object_ref in &object_refs {
        if Some(*object_ref) == pdf.encryption_ref() {
            continue;
        }

        // QDF: never emit object 0 or any free/deleted entry as a body object
        // (qpdf --qdf parity, flpdf-9hc.6.10). The xref free-list head and any
        // free rows are still written into the regenerated `xref` table below.
        if options.qdf && (object_ref.number == 0 || qdf_skip_refs.contains(object_ref)) {
            continue;
        }

        // ── Step 4: skip members that will be routed into an ObjStm batch ───
        if member_to_batch.contains_key(object_ref) {
            continue;
        }

        // QDF length-holder objects from a prior qdf pass are reconstructed
        // (with the recomputed length) below; do not re-emit them here as
        // ordinary integer objects, or idempotence breaks.
        if options.qdf && existing_holders.contains(&object_ref.number) {
            continue;
        }

        // Resolve the object; propagate the error so callers see corrupt input
        // rather than getting a silent success with missing /Root descendants.
        let mut object = pdf.resolve(*object_ref)?;

        // flpdf-9hc.4.9: encrypt every string inside this object's resolved
        // graph. Stream PAYLOAD encryption happens later (after the compress
        // policy reencode), and the /Encrypt dict object itself is exempt per
        // PDF 1.7 §7.6.1 ("strings and streams inside the encryption
        // dictionary are not encrypted").
        if let Some(ctx) = &encrypt_ctx {
            if *object_ref != ctx.encrypt_ref {
                encrypt_strings_in_object_for_writer(*object_ref, &mut object, ctx)?;
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

        // Duplicate detection (same contract as write_qdf).
        if offsets.contains_key(&object_ref.number) {
            return Err(crate::Error::Unsupported(format!(
                "duplicate object number {} in xref table",
                object_ref.number
            )));
        }

        // QDF per-object comment: "%% Original object ID: N G"
        // Emitted immediately before the "N G obj" line so human readers can
        // locate objects without consulting the xref table.  Mirrors qpdf
        // 11.9.0 --qdf output.  Suppressed when no_original_object_ids=true.
        // The xref offset below is recorded AFTER the comment so it still
        // points at the "N G obj" line, not at the comment.
        if options.qdf && !options.no_original_object_ids {
            bytes.extend_from_slice(
                format!(
                    "%% Original object ID: {} {}\n",
                    object_ref.number, object_ref.generation
                )
                .as_bytes(),
            );
        }

        let emit_offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", object_ref.number, object_ref.generation).as_bytes(),
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
            let mut reencoded = match effective_stream_policy(options) {
                Some(compress_policy) => apply_stream_compress_policy(&stream, compress_policy),
                // Preserve mode: pass dict + raw bytes verbatim, no decode/re-encode.
                // `stream` is owned (moved out of the resolved `object`) and is
                // not used after this match, so we can move it directly instead
                // of cloning the (potentially large) data buffer.
                None => Object::Stream(stream),
            };

            // flpdf-9hc.4.9: encrypt the stream payload AFTER any filter
            // re-encoding, so the encryption operates on the on-disk bytes.
            // /Length is updated to the encrypted byte count (AES-CBC adds a
            // 16-byte IV prefix and PKCS#7 padding). Skip for the /Encrypt
            // object itself.
            if let Some(ctx) = &encrypt_ctx {
                if *object_ref != ctx.encrypt_ref {
                    if let Object::Stream(ref mut s) = reencoded {
                        encrypt_stream_payload_for_writer(*object_ref, s, ctx)?;
                    }
                }
            }

            if let Object::Stream(ref s) = reencoded {
                if options.qdf {
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
                    write_stream_to_buf(&mut bytes, s, options.newline_before_endstream);
                }
            } else if options.qdf {
                reencoded.write_pdf_qdf(&mut bytes, 0);
            } else {
                reencoded.write_pdf(&mut bytes);
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
        offsets.insert(object_ref.number, (object_ref.generation, emit_offset));
    }

    // ── Step 5: emit each ObjStm container ───────────────────────────────────
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_ref = container_refs[batch_idx];
        let body = object_streams::emit_objstm_body(pdf, batch)?;
        let stream = object_streams::wrap_objstm_body(&body, options.compress_streams)?;

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
            trailer.insert("Size", Object::Integer(object_count as i64));
            trailer.insert("Root", Object::Reference(root_ref));
            apply_encrypt_trailer_entries(&mut trailer, pdf, options, encrypt_ctx.as_ref());

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
                write_qdf_trailer(&mut bytes, &trailer);
                bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
            } else {
                bytes.extend_from_slice(b"trailer\n");
                trailer.write_pdf(&mut bytes);
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
                    member_to_batch.get(&ObjectRef::new(number, 0))
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
            if pdf.is_encrypted() {
                xref_dict.remove("Encrypt");
            }
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
            xref_dict.insert("Root", Object::Reference(root_ref));

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
            if options.static_id {
                apply_static_id(&mut xref_dict);
            } else {
                apply_random_id(&mut xref_dict);
            }

            let xref_stream = crate::Stream::new(xref_dict, stream_data);
            bytes.extend_from_slice(format!("{xref_object_number} 0 obj\n").as_bytes());
            write_stream_to_buf(&mut bytes, &xref_stream, options.newline_before_endstream);
            bytes.extend_from_slice(b"\nendobj\n");
            bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        }
    }

    out.write_all(&bytes)?;
    Ok(())
}

/// Apply the stream compression policy to a single stream object.
///
/// This is the choke-point for re-emitting **regular indirect stream
/// objects** in the full-rewrite path. The cross-reference stream and
/// object-stream (ObjStm) containers apply the same `CompressStreams`
/// policy on their own dedicated branches (the xref-stream branch below
/// and [`object_streams::wrap_objstm_body`]); they do not flow through
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
/// toggle (the held-for-review zlib-parity decision, flpdf-9hc.12.5).
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

/// Write a PDF stream to `buf`, applying the [`NewlineBeforeEndstream`] policy.
///
/// This is the **single choke-point** through which all stream emission in the
/// full-rewrite writer paths flows.  It mirrors the layout that
/// [`Object::Stream::write_pdf`] produces, but gives the caller control over
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
    buf.extend_from_slice(b"\nstream\n");
    buf.extend_from_slice(&stream.data);

    match policy {
        NewlineBeforeEndstream::Yes => {
            // Always write exactly one newline before endstream.
            buf.push(b'\n');
        }
        NewlineBeforeEndstream::No => {
            // Only write a newline when the payload does not already end with one.
            let ends_with_eol = stream
                .data
                .last()
                .map(|&b| b == b'\n' || b == b'\r')
                .unwrap_or(false);
            if !ends_with_eol {
                buf.push(b'\n');
            }
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
/// raw decoded length — so the writer and `fix_qdf` mesh (flpdf-9hc.6.12).
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
    buf.extend_from_slice(b"\nstream\n");
    buf.extend_from_slice(&stream.data);

    match policy {
        NewlineBeforeEndstream::Yes => {
            buf.push(b'\n');
        }
        NewlineBeforeEndstream::No => {
            let ends_with_eol = stream
                .data
                .last()
                .map(|&b| b == b'\n' || b == b'\r')
                .unwrap_or(false);
            if !ends_with_eol {
                buf.push(b'\n');
            }
        }
    }

    buf.extend_from_slice(b"endstream");
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
fn write_qdf_trailer(bytes: &mut Vec<u8>, trailer: &Dictionary) {
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
        value.write_pdf(bytes);
        bytes.push(b'\n');
    }

    bytes.extend_from_slice(b">>\n");
}

#[cfg(test)]
mod tests {
    use super::*;

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
            matches!(pdf.resolve(plain_dict).unwrap(), Object::Dictionary(_)),
            "obj 3 must resolve as a plain dictionary"
        );
        assert!(
            matches!(pdf.resolve(stream_ref).unwrap(), Object::Stream(_)),
            "obj 4 must resolve as a stream"
        );
        assert!(
            matches!(pdf.resolve(gen1_ref).unwrap(), Object::Dictionary(_)),
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
            matches!(pdf.resolve(m0).unwrap(), Object::Dictionary(_)),
            "obj 2 must resolve from the xref stream as a plain dictionary"
        );
        assert!(
            matches!(pdf.resolve(m1).unwrap(), Object::Dictionary(_)),
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

        // build_partition_fixture has a real content stream (obj 4 = b"hello"),
        // so encryption actually exercises AES stream IV generation. minimal.pdf
        // has no streams and no encryptable strings, so it would never emit an
        // IV and the zero-IV assertion below would be vacuous.
        let fixture = build_partition_fixture();

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
}
