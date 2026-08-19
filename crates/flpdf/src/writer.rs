//! qpdf correspondence: QPDFWriter.cc responsibilities shared with writer submodules and linearization.
#[path = "writer/encrypted_strings.rs"]
pub(crate) mod encrypted_strings;
#[path = "writer/encryption_state.rs"]
pub(crate) mod encryption_state;
#[path = "writer/object_streams.rs"]
pub(crate) mod object_streams;
#[path = "writer/pclm.rs"]
pub(crate) mod pclm;
mod pdf_writer;
#[path = "writer/plain/mod.rs"]
pub(crate) mod plain;
#[path = "writer/serialize.rs"]
pub(crate) mod serialize;
mod settings;
pub use object_streams::ObjectStreamMode;
pub use pdf_writer::{PdfWriter, WriterConfiguration};
#[cfg(test)]
use serialize::framing_adds_newline as stream_framing_adds_newline;
use serialize::write_stream_payload;
pub use serialize::write_stream_to_buf;
#[cfg(test)]
use serialize::write_stream_with_id_writer as write_stream_to_buf_with_id_writer;
pub use settings::DecodeLevel;

/// Test-only convenience for exercising the canonical qpdf writer lifecycle
/// from crate-internal unit suites. This deliberately has no public alias for
/// the removed free writer routes.
#[cfg(test)]
pub(crate) fn write_qpdf_to_memory<R, F>(pdf: &mut Pdf<R>, configure: F) -> Result<Vec<u8>>
where
    R: Read + Seek + 'static,
    F: FnOnce(&mut PdfWriter<'_, R>),
{
    let mut writer = PdfWriter::new(pdf);
    configure(&mut writer);
    writer.set_output_memory()?;
    writer.write()?;
    writer.get_buffer()
}

use crate::object_handle::ObjectValue;
use crate::pdf_version::{parse_pdf_version, PdfVersion, PDF_1_2, PDF_1_5};
use crate::pipeline::{Pipeline, PlString};
use crate::{
    filters, Dictionary, Object, ObjectHandle, ObjectRef, Pdf, Result, XrefEntry, XrefForm,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

/// Result data produced by a completed full-rewrite emitter.
///
/// Both maps are assembled while writing the output. They deliberately do not
/// consult the source xref table: the caller observes the objects and xref
/// records that this emitter actually placed in the new file.
#[derive(Clone, Debug, Default)]
pub(crate) struct WriterResult {
    pub(crate) old_to_new: BTreeMap<ObjectRef, ObjectRef>,
    pub(crate) written_xref: BTreeMap<ObjectRef, XrefEntry>,
}

impl WriterResult {
    pub(crate) fn new(
        old_to_new: BTreeMap<ObjectRef, ObjectRef>,
        written_xref: BTreeMap<ObjectRef, XrefEntry>,
    ) -> Self {
        Self {
            old_to_new,
            written_xref,
        }
    }
}

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
/// When configured on [`PdfWriter`], it **overrides** the writer's compression setting
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
/// When `PdfWriter` is configured with a stream-data mode, it takes precedence
/// over the writer's compression setting for per-object stream bodies.
/// Linearized output also applies the resulting global compression choice to
/// its generated hint, object, and cross-reference streams, matching qpdf.
///
/// # Interaction with QDF mode
///
/// When [`PdfWriter`] is configured for QDF, QDF wins: every applicable stream is
/// decoded to raw bytes (equivalent to `Uncompress`), overriding even
/// `stream_data = Some(Preserve)`.  This matches qpdf's behaviour where `--qdf`
/// takes precedence over `--stream-data=preserve`.
///
/// # Default
///
/// The default is `None` — no stream-data mode is set — which leaves the
/// writer's compression setting in control.
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
/// 1. Legacy QDF mode (`options.qdf`) returns `Some(CompressStreams::No)` —
///    QDF requires fully decoded streams regardless of `stream_data`. The
///    PdfWriter bridge precomputes qpdf's setter-aware QDF defaults instead.
/// 2. `options.stream_data = Some(mode)` overrides `options.compress_streams`.
/// 3. `options.stream_data = None` falls back to `options.compress_streams`.
pub(crate) fn effective_stream_policy(options: &WriterOptions) -> Option<CompressStreams> {
    if options.qdf && !options.qdf_stream_policy_precomputed {
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
/// - [`Never`](Self::Never) (the **flpdf default**) — never insert a newline;
///   exactly the raw payload bytes sit between `stream` and `endstream`. This
///   reproduces qpdf's **default** output (qpdf only inserts a newline when run
///   with `--newline-before-endstream`), and is required for byte-identical
///   `qpdf`-equivalent output.
/// - [`Yes`](Self::Yes) — always write exactly one `b'\n'`, satisfying the ISO
///   32000-1 §7.3.8.1 recommendation and easing hand-editing. Equivalent to
///   qpdf run **with** `--newline-before-endstream`.
/// - [`No`](Self::No) — write a single `b'\n'` only when the payload does not
///   already end with `\n`/`\r`; if it does, `endstream` is adjacent. This is a
///   flpdf-specific middle ground and matches neither of qpdf's two modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum NewlineBeforeEndstream {
    /// Always write exactly one `b'\n'` before `endstream`, regardless of
    /// whether the payload already ends with a newline.
    ///
    /// Satisfies ISO 32000-1 §7.3.8.1 and matches qpdf run with
    /// `--newline-before-endstream`.
    Yes,
    /// Write a single `b'\n'` before `endstream` only when the payload does not
    /// already end with `\n`; otherwise `endstream` is adjacent. Payloads
    /// ending with bare `\r` or `\r\n` still receive an added `\n`.
    ///
    /// Matches qpdf's `(last_char != '\n')` check in QPDFWriter.cc:1560,
    /// which is what QDF form falls back to when the caller does not set
    /// [`NewlineBeforeEndstream::Yes`] explicitly.
    No,
    /// Never insert a newline: the raw payload is written verbatim and
    /// `endstream` follows immediately, so exactly `/Length` bytes sit between
    /// `stream` and `endstream` (the **flpdf default**).
    ///
    /// Reproduces qpdf's default output and is required for byte-identical
    /// qpdf-equivalent rewrites.
    #[default]
    Never,
}

/// Fixed V=5 R=5/R=6 secret material for qpdf-compatible test/helper writes.
///
/// This type is compiled only for crate unit tests and the `qpdf-zlib-compat`
/// test feature. The byte order matches qpdf 11.9.0's four random draws:
/// 32-byte file key, 16 bytes of `/U` salts, 16 bytes of `/O` salts, and the
/// 4-byte `/Perms` tail. Production writes do not expose this field and keep
/// using the OS CSPRNG.
#[cfg(any(test, feature = "qpdf-zlib-compat"))]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V5Randomness {
    /// 32-byte file encryption key.
    pub file_key: [u8; 32],
    /// 8-byte user-password validation salt.
    pub user_validation_salt: [u8; 8],
    /// 8-byte user-password key-derivation salt.
    pub user_key_salt: [u8; 8],
    /// 8-byte owner-password validation salt.
    pub owner_validation_salt: [u8; 8],
    /// 8-byte owner-password key-derivation salt.
    pub owner_key_salt: [u8; 8],
    /// 4 bytes appended to the `/Perms` plaintext block.
    pub perms_random_tail: [u8; 4],
}

#[cfg(any(test, feature = "qpdf-zlib-compat"))]
impl V5Randomness {
    /// Split one qpdf-ordered 68-byte random input into the V=5 fields.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 68]) -> Self {
        Self {
            file_key: std::array::from_fn(|index| bytes[index]),
            user_validation_salt: std::array::from_fn(|index| bytes[32 + index]),
            user_key_salt: std::array::from_fn(|index| bytes[40 + index]),
            owner_validation_salt: std::array::from_fn(|index| bytes[48 + index]),
            owner_key_salt: std::array::from_fn(|index| bytes[56 + index]),
            perms_random_tail: std::array::from_fn(|index| bytes[64 + index]),
        }
    }
}

/// Shared callback storage used by the qpdf-shaped writer lifecycle.
///
/// `WriterOptions` is cloneable because the full-rewrite preflight creates
/// short-lived option snapshots. Keeping the callback behind shared interior
/// mutability preserves that property while still allowing each snapshot to
/// report to the one registered qpdf progress reporter.
type ProgressCallback = Box<dyn FnMut(u8) + 'static>;
type SharedProgressCallback = Rc<RefCell<ProgressCallback>>;
type SharedProgressState = Rc<RefCell<ProgressStateInner>>;

#[derive(Clone)]
pub(crate) struct ProgressReporter {
    callback: SharedProgressCallback,
    state: SharedProgressState,
}

impl ProgressReporter {
    pub(crate) fn new(reporter: Box<dyn FnMut(u8) + 'static>) -> Self {
        Self {
            callback: Rc::new(RefCell::new(reporter)),
            state: Rc::new(RefCell::new(ProgressStateInner::default())),
        }
    }

    pub(crate) fn report(&self, percent: u8) {
        (self.callback.borrow_mut())(percent);
    }

    pub(crate) fn configure(&self, events_expected: usize) {
        *self.state.borrow_mut() = ProgressStateInner {
            events_expected: events_expected.max(1),
            ..ProgressStateInner::default()
        };
    }

    /// Translate QPDFWriter::indicateProgress (`QPDFWriter.cc:2957-2982`).
    ///
    /// The counter is shared because the canonical writer clones its option
    /// snapshot while a linearized file performs both passes. The callback is
    /// invoked after the state borrow is released so a reporter can safely
    /// observe external state without extending the writer's interior borrow.
    pub(crate) fn indicate(&self, decrement: bool, finished: bool) {
        let progress = {
            let mut state = self.state.borrow_mut();
            if decrement {
                state.events_seen = state.events_seen.saturating_sub(1);
                return;
            }

            state.events_seen = state.events_seen.saturating_add(1);
            let progress = if finished {
                Some(100)
            } else if state.events_seen >= state.next_progress_report {
                Some(if state.next_progress_report == 0 {
                    0
                } else {
                    let scaled = state.events_seen.saturating_mul(100) / state.events_expected;
                    1_u8.saturating_add(u8::try_from(scaled.min(98)).unwrap_or(98))
                })
            } else {
                None
            };

            let increment = (state.events_expected / 100).max(1);
            while state.events_seen >= state.next_progress_report {
                state.next_progress_report = state.next_progress_report.saturating_add(increment);
            }
            progress
        };

        if let Some(progress) = progress {
            self.report(progress);
        }
    }
}

impl fmt::Debug for ProgressReporter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProgressReporter(..)")
    }
}

#[derive(Debug)]
struct ProgressStateInner {
    events_expected: usize,
    events_seen: usize,
    next_progress_report: usize,
}
impl Default for ProgressStateInner {
    fn default() -> Self {
        Self {
            events_expected: 1,
            events_seen: 0,
            next_progress_report: 0,
        }
    }
}

/// Internal options shared by the canonical writer and writer-owned tests.
/// Public callers configure [`PdfWriter`] directly.
#[derive(Debug, Default, Clone)]
pub(crate) struct WriterOptions {
    /// Stream decode level used by the qpdf-shaped writer bridge.
    ///
    /// A filter chain that is not wholly decodable at this level is preserved
    /// as a whole, matching QPDF_Stream's all-or-nothing filterability
    /// decision.
    pub decode_level: DecodeLevel,

    /// Normalize decoded page-content streams using qpdf token rules.
    ///
    /// This applies to direct streams in a page `/Contents` value and terminal
    /// indirect streams reached from page `/Contents`; other streams retain
    /// their decoded bytes unchanged. PdfWriter enables it implicitly for QDF
    /// output unless the caller explicitly disables it.
    pub content_normalization: bool,

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
    /// The canonical rewrite honors this flag. It is mutually exclusive with
    /// [`WriterOptions::static_id`] and is rejected for encrypted output (the
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
    /// and string-level AES encryption use the same fixed initialization
    /// vector qpdf's `--static-aes-iv` uses — byte `i` is `14 * (1 + i)` —
    /// making the ciphertext deterministic and comparable with qpdf's output.
    /// Without this flag (the default `false`) every encryption call generates
    /// a fresh random IV via the OS CSPRNG.
    ///
    /// Under CBC the vector is written at the head of the ciphertext, so it is
    /// part of the output bytes: a different vector means a different file.
    /// Must never be set in production code; deterministic IVs make AES CBC
    /// completely insecure.
    pub static_aes_iv: bool,

    /// Fixed V=5 security-handler random bytes for qpdf byte-gate helpers.
    ///
    /// This field exists only in crate unit tests and builds with the
    /// `qpdf-zlib-compat` test feature. It is deliberately not a CLI or
    /// production seed option. `None` preserves the production OS CSPRNG.
    #[cfg(any(test, feature = "qpdf-zlib-compat"))]
    #[doc(hidden)]
    pub v5_randomness: Option<V5Randomness>,

    /// Enforce a minimum PDF version in the output header.
    ///
    /// The effective version is `max(source_version, min_version)`. Format:
    /// `"1.3"`, `"1.7"`, etc.
    ///
    /// Mirrors `qpdf --min-version`.
    pub min_version: Option<String>,

    /// Enforce a minimum Adobe extension level in the output catalog's
    /// `/Extensions /ADBE /ExtensionLevel`.
    ///
    /// Combined with [`Self::min_version`] via qpdf's pairwise rule: a higher
    /// `min_version` **resets** the extension level (does not carry it across a
    /// version bump). When the resulting effective level is greater than 0, the
    /// writer injects
    /// `/Extensions << /ADBE << /BaseVersion /<ver> /ExtensionLevel <lvl> >> >>`
    /// into the Catalog on the full-rewrite path. When 0, no injection (existing
    /// Catalog untouched).
    ///
    /// Mirrors qpdf's `--min-version <version>-<level>` (the level portion) and
    /// the extension_level `QPDFJob` accumulates into `max_input_version` from
    /// every opened input's Catalog.
    pub min_extension_level: Option<i64>,

    /// Force the output PDF version header to exactly this value, ignoring the
    /// source version and the linearize floor.
    ///
    /// Mirrors `qpdf --force-version`.
    pub force_version: Option<String>,

    /// Adobe extension level paired with [`Self::force_version`].
    ///
    /// qpdf treats the version and extension level as one forced pair when
    /// deciding whether encryption remains compatible and when reconciling
    /// the Catalog's `/Extensions /ADBE` entry.
    pub force_extension_level: Option<i64>,

    /// Text written immediately after qpdf's binary or PCLm header marker.
    ///
    /// [`PdfWriter::set_extra_header_text`](crate::PdfWriter::set_extra_header_text)
    /// normalizes this value to end in one newline, matching qpdf.
    pub extra_header_text: String,

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
    /// The canonical rewrite emits `%% Original object ID: N G` immediately
    /// before each indirect object's `N G obj` line when `qdf = true` and this
    /// flag is `false`. Setting this flag to `true` suppresses those comments
    /// while leaving the `N G obj` lines intact — matching qpdf's
    /// `--no-original-object-ids` behaviour exactly.
    pub no_original_object_ids: bool,

    /// Object stream emission policy for the output.
    ///
    /// Mirrors `qpdf --object-streams=preserve|disable|generate`. Defaults to
    /// [`ObjectStreamMode::Preserve`], matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    ///
    /// The canonical rewrite consults this setting whenever it emits ObjStms.
    pub object_streams: ObjectStreamMode,

    /// Preserve source objects that are not reachable from the trailer roots.
    ///
    /// The plain emitter currently honors it only with
    /// [`ObjectStreamMode::Disable`]; object-stream membership for Preserve and
    /// Generate remains a bounded follow-up.
    pub preserve_unreferenced_objects: bool,

    /// Stream compression policy for the full-rewrite path.
    ///
    /// [`CompressStreams::Yes`] (the default) decodes each stream and
    /// re-encodes it with a single `/FlateDecode` filter, matching qpdf's
    /// default behaviour.  [`CompressStreams::No`] decodes each stream and
    /// emits the raw bytes without any filter; streams that cannot be decoded
    /// (e.g. `DCTDecode`/`JPXDecode` image data) are passed through verbatim.
    ///
    /// It governs regular indirect streams, ObjStm containers, and the xref
    /// stream alike.
    pub compress_streams: CompressStreams,

    /// Whether to insert a newline immediately before each `endstream` keyword.
    ///
    /// ISO 32000-1 §7.3.8.1 recommends an end-of-line marker before `endstream`.
    /// [`NewlineBeforeEndstream::Never`] (the default) never inserts one, so
    /// exactly `/Length` bytes sit between `stream` and `endstream` — matching
    /// qpdf's default output and required for byte-identical qpdf-equivalent
    /// rewrites. [`NewlineBeforeEndstream::Yes`] always writes exactly one
    /// `b'\n'` before `endstream`, matching qpdf run with
    /// `--newline-before-endstream`. [`NewlineBeforeEndstream::No`] omits the
    /// extra newline only when the stream payload already ends with exactly
    /// `\n` (matches qpdf's `(last_char != '\n')` check; bare `\r` or `\r\n`
    /// endings still receive an added `\n`).
    ///
    /// The `/Length` value in the stream dictionary is **not** affected by this
    /// setting — it always reflects the raw payload byte count only.
    ///
    /// Applied to every stream in the canonical rewrite output.
    pub newline_before_endstream: NewlineBeforeEndstream,

    /// Emit the document in QDF (Query Data Format) mode.
    ///
    /// When `true`, every stream that uses a
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
    /// This field is the internal emitter representation of QDF mode.
    ///
    /// [`FlateDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`LZWDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`ASCIIHexDecode`]: https://pdf.pizza/spec/7.4.2
    /// [`ASCII85Decode`]: https://pdf.pizza/spec/7.4.3
    /// [`RunLengthDecode`]: https://pdf.pizza/spec/7.4.5
    /// [`compress_streams`]: WriterOptions::compress_streams
    pub qdf: bool,

    /// Whether the PdfWriter lifecycle already applied qpdf's setter-aware
    /// QDF stream defaults.
    pub(crate) qdf_stream_policy_precomputed: bool,

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
    /// [`compress_streams`]: WriterOptions::compress_streams
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

    /// Encrypt the canonical output with the supplied [`crate::EncryptParams`]
    /// (qpdf `--encrypt …` equivalent).
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
    /// - `qdf` may be enabled; encrypted strings and stream dictionaries retain
    ///   QDF layout while their encrypted bytes remain ciphertext.
    pub encrypt: Option<crate::encrypt_setup::EncryptParams>,

    /// Copy the authenticated encryption parameters from a donor PDF and
    /// re-use its file encryption key (qpdf `--copy-encryption`
    /// equivalent).
    ///
    /// When set the writer bypasses the normal password-derivation path and
    /// constructs an `EncryptionContext` directly from the pre-recovered file
    /// key, the donor's Standard handler values, and the donor's `/ID[0]`.
    /// qpdf's canonical copy rules are applied: V4 is emitted as AESV2 even
    /// when the donor used RC4, and V5 is emitted as AESV3.
    ///
    /// Exactly one of `encrypt` and `copy_encryption` may be set; the CLI
    /// enforces mutual exclusion via `conflicts_with`.  The writer asserts this
    /// invariant at the top of the full-rewrite path.
    ///
    /// V=1/V=2 RC4, V=4 AESV2, and V=5 R=5/R=6 AESV3 donors are supported by
    /// the canonical writer.
    pub copy_encryption: Option<crate::encrypt_setup::CopyEncryptionSource>,

    /// Emit qpdf's PCLm-oriented object order and header.
    pub pclm: bool,

    /// qpdf progress callback shared by the lifecycle bridge and the emitter.
    pub(crate) progress_reporter: Option<ProgressReporter>,
}

/// Configure qpdf-shaped progress after the writer has completed the setup
/// that allocates any synthetic objects. qpdf snapshots
/// `QPDF::getObjectCount()` only after `doWriteSetup` (QPDFWriter.cc:2189-2193),
/// so callers pass the number of fresh ObjStm containers allocated during that
/// setup without mutating the source document just for progress accounting.
pub(crate) fn configure_progress_for_pdf<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    additional_objects: usize,
    linearized: bool,
) -> Result<()> {
    if options.progress_reporter.is_none() {
        return Ok(());
    }

    // cov:ignore-start: Pdf::get_object_count returns u32, and flpdf's
    // supported targets have usize at least 32 bits wide.
    let object_count = usize::try_from(pdf.get_object_count()?).map_err(|_| {
        crate::Error::Unsupported("PdfWriter progress object count does not fit usize".into())
    })?;
    // cov:ignore-end
    configure_progress(
        options,
        object_count.saturating_add(additional_objects),
        linearized,
    );
    Ok(())
}

pub(crate) fn configure_progress(options: &WriterOptions, object_count: usize, linearized: bool) {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.configure(object_count.saturating_mul(if linearized { 2 } else { 1 }));
    } // cov:ignore: LLVM maps this closing brace as an executable branch line
}

pub(crate) fn report_progress_event(options: &WriterOptions) {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.indicate(false, false);
    }
}

pub(crate) fn decrement_progress_event(options: &WriterOptions) {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.indicate(true, false);
    }
}

pub(crate) fn report_progress_finished(options: &WriterOptions) {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.indicate(false, true);
    }
}

/// True when `--force-version` pins the output header below PDF 1.5.
///
/// Object streams and cross-reference streams were both introduced in PDF 1.5.
/// qpdf treats a forced version as a hard cap it will not exceed, so when the
/// forced header is below 1.5 it suppresses those features entirely and falls
/// back to a classic xref table (observed on qpdf 11.9.0). `--min-version` is
/// only a floor — it never triggers this, because the 1.5 object-stream floor
/// raises above it — so this checks `force_version` specifically. An invalid
/// (unparseable) `force_version` is ignored, matching [`effective_pdf_version`].
pub(crate) fn force_version_below_1_5(options: &WriterOptions) -> bool {
    options
        .force_version
        .as_deref()
        .and_then(parse_pdf_version)
        .is_some_and(|version| version < PDF_1_5)
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
pub(crate) fn effective_pdf_version<'a>(
    source: &'a str,
    options: &'a WriterOptions,
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

    // Apply encryption floor (mirrors qpdf QPDFWriter::setEncryptionParametersInternal
    // at QPDFWriter.cc L806-815). AES-256 (R>=6), AES-256 legacy (R=5), AES-128
    // (R=4), RC4-128 (R=3, or R=4 without AES), RC4-40 (R<3) each require a
    // minimum header version.
    let enc_floor = encryption_version_floor(options);
    if let Some(encryption_floor) = enc_floor {
        if encryption_floor > best {
            best = encryption_floor;
        }
    }

    // Apply object-stream floor (object streams require >= 1.5).
    if object_streams && PDF_1_5 > best {
        best = PDF_1_5;
    }

    // Apply linearize floor (PDF spec requires >= 1.2).
    if linearize && PDF_1_2 > best {
        best = PDF_1_2;
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
    // Encryption floor matched — return a static string for the emitted version.
    // cov:ignore-start: inner-if closing braces are llvm-cov region artifacts;
    // the `return` inside is exercised by
    // effective_pdf_version_folds_each_encryption_floor_arm.
    if let Some(encryption_floor) = enc_floor {
        if encryption_floor == best {
            return best.static_version_str().unwrap_or("1.7");
        }
    }
    // cov:ignore-end
    // Object-stream floor "1.5" — reached when best == (1,5) and neither source
    // nor min_version nor encryption floor matched.
    if best == PDF_1_5 {
        return "1.5";
    }
    // Linearize floor "1.2" — only reached when best == (1,2) and neither
    // source nor min_version nor encryption floor matched.
    "1.2"
}

/// Header-version floor imposed by the encryption method requested via
/// [`WriterOptions::encrypt`] / [`WriterOptions::copy_encryption`].
///
/// Mirrors qpdf QPDFWriter.cc L806-815 (`setEncryptionParametersInternal`):
///
/// | Method                       | Floor (version, ext) |
/// |------------------------------|----------------------|
/// | V=5 R=6 AES-256              | (1.7, 8)             |
/// | V=5 R=5 AES-256 (legacy)     | (1.7, 3)             |
/// | V=4 R=4 AES-128              | (1.6, 0)             |
/// | V=4 R=4 RC4-128              | (1.5, 0)             |
/// | V=2 R=3 RC4-128              | (1.4, 0)             |
/// | V=1 R=2 RC4-40               | (1.3, 0)             |
/// | `copy_encryption`             | derived from copied V/R and AES mode |
///
/// qpdf's copy path forces AES for V>=4, so a copied V=4 source has the AESV2
/// floor even when the donor used RC4.
fn encryption_version_floor(options: &WriterOptions) -> Option<PdfVersion> {
    use crate::encrypt_setup::EncryptMethod;
    if let Some(ref enc) = options.encrypt {
        return Some(match enc.method {
            EncryptMethod::V5R6Aes256 => PdfVersion::new(1, 7, 8),
            EncryptMethod::V5R5Aes256 => PdfVersion::new(1, 7, 3),
            EncryptMethod::V4Aes128 => PdfVersion::new(1, 6, 0),
            EncryptMethod::V4Rc4128 => PdfVersion::new(1, 5, 0),
            EncryptMethod::V2Rc4128 => PdfVersion::new(1, 4, 0),
            EncryptMethod::V1Rc440 => PdfVersion::new(1, 3, 0),
        });
    }
    if let Some(source) = options.copy_encryption.as_ref() {
        let version = source.encrypt_dict.get("V")?.as_integer()?;
        let revision = source.encrypt_dict.get("R")?.as_integer()?;
        return Some(if revision >= 6 {
            PdfVersion::new(1, 7, 8)
        } else if version >= 5 && revision >= 5 {
            PdfVersion::new(1, 7, 3)
        } else if version == 4 || revision >= 4 {
            // copyEncryptionParameters forces AES for all V>=4.
            PdfVersion::new(1, 6, 0)
        } else if revision >= 3 || version == 2 {
            PdfVersion::new(1, 4, 0)
        } else {
            PdfVersion::new(1, 3, 0)
        });
    }
    None
}

/// Compute the effective (PDF version, Adobe extension level) pair to write,
/// applying qpdf's pairwise combined rule (`QPDFWriter::setMinimumPDFVersion`):
///
/// * `options.min_version` unset → take `(source, source_ext)`.
/// * new version > current → take both from the new source. The extension
///   level resets across a version bump; it does not carry across.
/// * new version == current AND new ext > current → take ext only.
/// * new version < current → ignore.
///
/// This is the pair-aware sibling of [`effective_pdf_version`] and delegates
/// to it for the version half. The extension level is only meaningful when
/// greater than zero; callers should injection-gate on that. `linearize` and
/// `object_streams` are threaded through unchanged.
pub(crate) fn effective_pdf_version_and_ext<'a>(
    source: &'a str,
    source_ext: i64,
    options: &'a WriterOptions,
    linearize: bool,
    object_streams: bool,
) -> (&'a str, i64) {
    // Version half: delegate.
    let ver = effective_pdf_version(source, options, linearize, object_streams);

    // Extension level half: pairwise. An input's extension level survives only
    // when that input's version *equals* the effective version — i.e. that
    // input won or tied on the version race. A bumped input (whose version was
    // outbid, including a min_version that beat the source outright) drops its
    // extension level; the pairwise rule does not carry ext across a version
    // bump. When only one side ties, its ext wins alone; when both tie
    // (source_ver == min_ver == ver) the higher of the two ext values wins.
    // When neither ties (e.g. the object-stream floor 1.5 or linearize floor
    // 1.2 bumped past both) the effective ext is 0.
    let ver_parsed = parse_pdf_version(ver);
    let source_parsed = parse_pdf_version(source);
    let min_parsed = options.min_version.as_deref().and_then(parse_pdf_version);
    // `--force-version` returns the forced value verbatim from
    // `effective_pdf_version`. qpdf treats a valid `--force-version` as an
    // exact version/extension pair: neither the source nor the caller-
    // supplied minimum extension level propagates across it.
    let forced = options
        .force_version
        .as_deref()
        .and_then(parse_pdf_version)
        .is_some();
    let enc_floor = encryption_version_floor(options);
    let source_contributes = !forced && ver_parsed.is_some() && ver_parsed == source_parsed;
    let min_contributes = !forced && ver_parsed.is_some() && ver_parsed == min_parsed;
    let enc_contributes = !forced
        && ver_parsed.is_some()
        && enc_floor.map(|version| PdfVersion::new(version.major(), version.minor(), 0))
            == ver_parsed;
    let min_ext = options.min_extension_level.unwrap_or(0);
    let enc_ext = enc_floor.map(PdfVersion::extension_level).unwrap_or(0);
    // Whichever inputs tie with the effective version each contribute their ext;
    // an input that was outbid contributes nothing. Multiple ties combine via
    // `max` — qpdf-equivalent when multiple setMinimumPDFVersion calls arrive
    // at the same version, the higher extension level wins the tie.
    let mut ext = 0i64;
    if source_contributes {
        ext = ext.max(source_ext);
    }
    if min_contributes {
        ext = ext.max(min_ext);
    }
    if enc_contributes {
        ext = ext.max(enc_ext);
    }
    if forced {
        (ver, options.force_extension_level.unwrap_or(0))
    } else {
        (ver, ext)
    }
}

/// Ensure the destination Catalog carries
/// `/Extensions << /ADBE << /BaseVersion /<version> /ExtensionLevel <lvl> >> >>`.
///
/// Mirrors qpdf's `QPDFWriter::addDeveloperExtension` handling
/// (QPDFWriter.cc L1355-1450): if the Catalog has no `/Extensions`, create a
/// direct dict carrying only `/ADBE`; if it has one (direct dict or indirect
/// reference), resolve it to a Dictionary and overwrite the `/ADBE` entry
/// only, leaving non-ADBE developer prefixes intact; write the resulting
/// Extensions dict back onto the Catalog inline as a direct value.
///
/// Callers must only invoke this when the effective extension level is > 0.
///
/// # Errors
///
/// - [`crate::Error::Missing`] if the input has no `/Root` in its trailer.
/// - Propagates canonical-handle resolution errors when materialising the
///   Catalog or an indirect `/Extensions` value.
pub(crate) fn inject_adbe_extension<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: &str,
    extension_level: i64,
) -> Result<()> {
    // cov:ignore-start: defensive /Root guard. Called from
    // emit_canonical_pdf (AFTER its own root_ref check has already
    // returned Missing("/Root")) and from
    // crate::linearization::writer::write_linearized (whose own
    // resolve_catalog_adbe_status pre-check treats a missing root as
    // `has_adbe: false, orphans_indirect_object: false` rather than
    // erroring, so this is that caller's actual root check) -- unreachable
    // in every fixture in either test module.
    let (root_ref, catalog) = writer_catalog_copy(pdf)?;
    // cov:ignore-end

    // qpdf's unparse path works on an unsafe top-level dictionary copy and
    // makes the `/Extensions` value direct before changing `/ADBE`. Copy only
    // the immediate entries here so a direct stream elsewhere in the Catalog
    // remains accepted, matching qpdf's `unsafeShallowCopy` boundary.
    let raw_extensions = catalog.try_get_key(b"/Extensions")?;
    let extensions = if let Some(entries) = raw_extensions.try_as_dictionary()? {
        ObjectHandle::dictionary(entries.into_iter().collect())
    } else {
        ObjectHandle::dictionary(Vec::new())
    };

    // Overwrite /ADBE wholesale, leaving any other developer prefix keys alone.
    let adbe = ObjectHandle::dictionary(vec![
        (
            b"/BaseVersion".to_vec(),
            ObjectHandle::name(version.as_bytes().to_vec()),
        ),
        (
            b"/ExtensionLevel".to_vec(),
            ObjectHandle::integer(extension_level),
        ),
    ]);
    extensions.replace_key(b"/ADBE", adbe)?;

    catalog.replace_key(b"/Extensions", extensions)?;
    pdf.set_object_handle(root_ref, catalog)?;
    Ok(())
}

/// Strip `/Extensions /ADBE` from the destination Catalog when the effective
/// extension level is 0. This complements [`inject_adbe_extension`] and
/// mirrors qpdf's removal branches (QPDFWriter.cc L1408 whole-`/Extensions`
/// removal and L1432 `/ADBE`-only removal). Fires for two related cases:
/// (1) a version race (min_version bump or ObjStm floor) drops the pairwise
/// ext to 0 but the source Catalog carries an `/ADBE` entry that would
/// otherwise survive; (2) the source Catalog carries a stale / malformed
/// `/ADBE` (no `/ExtensionLevel` or non-integer) even without a race — qpdf
/// removes it based on key existence, not `/ExtensionLevel` validity, so
/// flpdf must match to preserve byte parity.
///
/// Only touches `/ADBE`; any other developer-prefix keys under `/Extensions`
/// are preserved (matching qpdf's per-prefix handling). Drops `/Extensions`
/// itself when it becomes empty after ADBE removal.
///
/// # Errors
///
/// - Propagates [`Pdf::resolve`] errors when materialising the Catalog or an
///   indirect `/Extensions` value.
pub(crate) fn strip_adbe_extension<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    // cov:ignore-start: defensive /Root guard, mirroring
    // inject_adbe_extension's identical comment (same two callers:
    // emit_canonical_pdf and crate::linearization::writer::write_linearized).
    let (root_ref, catalog) = writer_catalog_copy(pdf)?;
    // cov:ignore-end
    let raw_extensions = catalog.try_get_key(b"/Extensions")?;
    let Some(entries) = raw_extensions.try_as_dictionary()? else {
        return Ok(());
    };
    let extensions = ObjectHandle::dictionary(entries.into_iter().collect());
    if !extensions.try_get_keys()?.contains(b"/ADBE".as_slice()) {
        return Ok(());
    }

    extensions.remove_key(b"/ADBE");
    if extensions.try_get_keys()?.is_empty() {
        catalog.remove_key(b"/Extensions");
    } else {
        catalog.replace_key(b"/Extensions", extensions)?;
    }
    pdf.set_object_handle(root_ref, catalog)?;
    Ok(())
}

/// Resolve and copy the live Catalog's immediate entries for writer-owned
/// output mutations.
///
/// The legacy writer used `Pdf::resolve`, which can return a stale materialized
/// cache entry after a canonical ObjectHandle mutation. qpdf's writer operates
/// on a live `QPDFObjectHandle::unsafeShallowCopy` instead, so this boundary
/// resolves the canonical root slot, makes a direct top-level dictionary copy,
/// and leaves the final replacement to `Pdf::set_object_handle`. The immediate
/// entries stay shared; callers replace only top-level keys, so nested direct
/// values—including streams—are not cloned or rejected.
fn writer_catalog_copy<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<(ObjectRef, ObjectHandle)> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };
    let source = pdf.get_object_handle(root_ref);
    pdf.resolve_object_handle(&source)?;
    let entries = source
        .try_as_dictionary()?
        .ok_or_else(|| crate::Error::Unsupported("Catalog is not a dictionary".to_string()))?;
    let catalog = ObjectHandle::dictionary(entries.into_iter().collect());
    Ok((root_ref, catalog))
}

/// Detect whether the destination Catalog carries `/Extensions /ADBE` in any
/// form (dict-valued or via indirect reference; regardless of `/ExtensionLevel`
/// presence or value).
///
/// Mirrors qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0` check
/// (QPDFWriter.cc L1387). Used as the strip trigger for `eff_ext == 0`: when
/// the effective extension level is zero, qpdf removes stale `/ADBE` whether
/// or not the source dict carried a valid `/ExtensionLevel`; the previous
/// `adobe_extension_level() > 0` gate only fired for positive integer
/// `/ExtensionLevel` and silently passed through malformed / partial /ADBE
/// entries.
///
/// # Errors
///
/// - Propagates canonical-handle resolution errors when materialising the
///   Catalog or an indirect `/Extensions` value.
fn catalog_has_extensions_adbe<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    // cov:ignore-start: defensive /Root guard. catalog_has_extensions_adbe is
    // only reached after emit_canonical_pdf passes its own root_ref check
    // on the same Pdf.
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    // cov:ignore-end
    let catalog = pdf.get_object_handle(root_ref);
    pdf.resolve_object_handle(&catalog)?;
    if !catalog.try_has_key(b"/Extensions")? {
        return Ok(false);
    }
    let extensions = catalog.try_get_key(b"/Extensions")?;
    if extensions.try_as_dictionary()?.is_none() {
        return Ok(false);
    }
    Ok(extensions.try_get_keys()?.contains(b"/ADBE".as_slice()))
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

fn strip_writer_trailer_history_keys(trailer: &mut Dictionary) {
    strip_xref_stream_trailer_keys(trailer);
    trailer.remove("Prev");
}

/// Remap the trailer's surviving indirect references to their Catalog-first
/// (new) numbers.
///
/// `/Root` is overwritten by the caller with the new root ref and `/Encrypt`
/// is written by [`apply_encrypt_trailer_handle_entries`], so both are left untouched
/// here. Every other indirect value (notably `/Info`, which the renumber walk
/// always seeds) is rewritten through `map`; a value absent from the map is an
/// error rather than a stale number leaking into the output.
#[cfg(test)]
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

    // Pass 2: remap indirect references nested inside a DIRECT dict/array trailer
    // value (e.g. a direct `/Info << /Held 5 0 R >>`). qpdf's `enqueueObjectsStandard`
    // enqueues each trimmed-trailer value "handling direct objects recursively",
    // so the renumber walk seeds these nested holders and the trailer must rewrite
    // them to their new numbers too. Real trailers carry their references at the
    // top level (`/Root`, `/Info`, `/Encrypt`) — already handled above and excluded
    // here by the dict/array type filter — plus scalars (`/Size`/`/Prev` integers)
    // and the direct `/ID` byte-string array (recursed as a no-op). Only the
    // (spec-violating) direct dict/array trailer value carries nested references;
    // `renumber_refs_in_place` errors on an unmapped one, refusing to emit a
    // dangling trailer reference.
    for value in trailer.values_mut() {
        if matches!(value, Object::Dictionary(_) | Object::Array(_)) {
            crate::rewrite_renumber::renumber_refs_in_place(value, map)?;
        }
    }
    Ok(())
}

fn remap_qpdf_trailer_refs_with_removed<
    R: Read + Seek,
    M: crate::rewrite_renumber::NewNumberLookup,
>(
    pdf: &mut Pdf<R>,
    trailer: &mut Dictionary,
    map: &M,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut object = Object::Dictionary(trailer.clone());
    crate::rewrite_renumber::renumber_qpdf_refs_in_place_with_removed(
        pdf,
        &mut object,
        map,
        removed_refs,
    )?;
    *trailer = object
        .into_dict()
        .expect("qpdf trailer rewrite preserves the dictionary variant");
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

#[cfg(test)]
fn build_xref_stream_bytes(
    offsets: &BTreeMap<u32, (u16, XrefEntry)>,
    ranges: &[(u32, u32)],
) -> Result<Vec<u8>> {
    let mut stream_data = Vec::new();
    for &(start, count) in ranges {
        let end = start.checked_add(count).ok_or_else(|| {
            crate::Error::Unsupported("xref stream range does not fit u32".to_string())
        })?;
        for object_number in start..end {
            let (generation, entry) = offsets.get(&object_number).ok_or_else(|| {
                crate::Error::Unsupported("xref stream is missing object entry".to_string())
            })?;
            let object_type = match (object_number, entry) {
                (0, _) | (_, XrefEntry::Free { .. }) => 0,
                (_, XrefEntry::Compressed { .. }) => 2,
                _ => 1,
            };
            stream_data.push(object_type);
            match entry {
                XrefEntry::Free { next } => {
                    stream_data.extend_from_slice(&u64::from(*next).to_be_bytes()[0..8]);
                    stream_data.extend_from_slice(&u32::from(*generation).to_be_bytes()[0..4]);
                }
                XrefEntry::Uncompressed { offset } => {
                    stream_data.extend_from_slice(&offset.to_be_bytes()[0..8]);
                    stream_data.extend_from_slice(&u32::from(*generation).to_be_bytes()[0..4]);
                }
                XrefEntry::Compressed { stream, index } => {
                    stream_data.extend_from_slice(&u64::from(*stream).to_be_bytes()[0..8]);
                    stream_data.extend_from_slice(&u32::to_be_bytes(*index)[0..4]);
                }
            }
        }
    }
    Ok(stream_data)
}

// ---------------------------------------------------------------------------
// Full-rewrite path: decode+re-encode every stream
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
/// - **Encrypted documents**: [`crate::PdfWriter`] follows qpdf and preserves
///   authenticated source encryption by default. Explicit
///   [`WriterOptions::encrypt`] or [`WriterOptions::copy_encryption`] settings
///   select a new encryption context.
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
mod _writer_doc_anchor {} // keeps the `emit_canonical_pdf` docstring above attached to its function.

// ── Encryption context (flpdf-9hc.4.9) ───────────────────────────────────────

/// How the writer derives per-object string/stream encryption key material.
///
/// Mirrors the reader's per-object dispatch (`EncryptionMode`): V<5 handlers
/// derive a per-object key via Algorithm 1, while V=5 uses the 32-byte file
/// key directly with AES-256 (no per-object derivation).
#[derive(Debug, Clone, Copy)]
pub(crate) enum WriteCipher {
    /// V=1/V=2/V=4: per-object key via Algorithm 1, then RC4 or AES-128
    /// (the [`ObjectKeyAlg`](crate::security::standard::ObjectKeyAlg) selects
    /// the `sAlT` salt and the resulting cipher).
    PerObject(crate::security::standard::ObjectKeyAlg),
    /// V=5 R=5/R=6: the 32-byte file key is used directly with AES-256-CBC.
    /// There is no Algorithm-1 per-object derivation.
    FileKeyAes256,
}

/// Per-write encryption state used when [`WriterOptions::encrypt`] or
/// [`WriterOptions::copy_encryption`] is set. Built once via
/// [`build_encryption_context`] or [`build_copy_encryption_context`] — at the
/// top of [`emit_canonical_pdf`] for the full-rewrite path, or inside
/// [`crate::linearization::writer::write_linearized_for_pdf_writer`] for linearized output
/// (`--encrypt` only; donor-copy/automatic source preservation are not yet
/// supported there) —
/// and consumed by the per-object emission loop + the trailer-build step.
pub(crate) struct EncryptionContext {
    /// Built `/Encrypt` dictionary (from a 4.1/4.2/4.3 builder).
    pub(crate) encrypt_dict: Dictionary,
    /// File encryption key derived from passwords + `/ID[0]` (Algorithm 2),
    /// or — for V=5 — the random 32-byte file key (FEK).
    pub(crate) file_key: Vec<u8>,
    /// How per-object string/stream key material is derived (V<5 per-object
    /// vs V=5 file-key-direct).
    pub(crate) cipher: WriteCipher,
    /// Standard handler algorithm version (`/V`) used to derive writer data keys.
    pub(crate) encryption_v: i32,
    /// Standard handler revision (`/R`) retained with the writer encryption state.
    pub(crate) encryption_r: i32,
    /// Indirect reference of the freshly-allocated `/Encrypt` object. The
    /// emission loop skips this ref so the `/Encrypt` dict itself stays
    /// plaintext (PDF 1.7 §7.6.1).
    pub(crate) encrypt_ref: ObjectRef,
    /// The 16-byte `/ID[0]` bytes that were fed into the file-key derivation.
    /// The output trailer's `/ID` array MUST start with these same bytes —
    /// readers re-derive the file key from `/ID[0]` to validate the password.
    pub(crate) id0: Vec<u8>,
    /// When `true`, all AES CBC IVs are forced to `[0u8; 16]` instead of
    /// being drawn from the OS CSPRNG.  Testing only — mirrors
    /// [`WriterOptions::static_aes_iv`].
    pub(crate) static_aes_iv: bool,
    /// Whether the `/Metadata` stream is encrypted alongside the rest of the
    /// document (mirrors [`crate::EncryptParams::encrypt_metadata`]). When `false`
    /// (qpdf `--cleartext-metadata`, V=4/V=5 only), the `/Metadata` stream in
    /// [`metadata_ref`](Self::metadata_ref) is left in the clear instead of
    /// being run through the cipher.
    pub(crate) encrypt_metadata: bool,
    /// Indirect reference of the document `/Catalog`'s `/Metadata` stream, when
    /// one exists AND `encrypt_metadata` is `false`. Used by the emission loop
    /// to exempt exactly that object from encryption. `None` whenever metadata
    /// is encrypted (the common case) or the document has no `/Metadata`.
    pub(crate) metadata_ref: Option<ObjectRef>,
}

/// Resolve the document `/Catalog`'s `/Metadata` indirect reference, if any.
/// Used to exempt the XMP metadata stream from encryption under
/// `--cleartext-metadata`.
///
/// `pub(crate)`: also used by [`crate::linearization::writer::write_linearized_for_pdf_writer`],
/// which needs the same `--cleartext-metadata` exemption for linearized output.
pub(crate) fn resolve_metadata_stream_ref<R: Read + Seek>(pdf: &mut Pdf<R>) -> Option<ObjectRef> {
    let root = pdf.root_ref()?;
    let root_handle = pdf.get_object_handle(root);
    pdf.resolve_object_handle(&root_handle).ok()?;
    let metadata = root_handle.try_get_key(b"/Metadata").ok()?;
    metadata.object_ref().or_else(|| metadata.as_reference())
}

/// `id0` is the `/ID[0]` bytes the file encryption key is derived from
/// (PDF 1.7 §7.6.3.3 Algorithm 2); the caller must have already decided this
/// value — typically extracted from [`generate_id_array`] — and must write the
/// SAME bytes into the output trailer's `/ID[0]`, since a reader re-derives
/// the file key from `/ID[0]` to validate the password. Taking it as a
/// parameter (rather than resolving it internally from `pdf`) lets a caller
/// that already finalized `/ID` elsewhere (the linearized writer, which must
/// settle `/ID`'s final width before its two-pass probe loop runs) feed that
/// SAME value in, instead of this function re-deriving an independent one —
/// mirrors qpdf's own `generateID()`-is-idempotent contract: `/ID` is
/// computed once, and encryption setup consumes that single value
/// (`QPDFWriter::setEncryptionParameters` calls `generateID()` itself before
/// deriving `/O`/`/U`, and `writeTrailer`'s later call is a no-op).
pub(crate) fn build_encryption_context(
    options: &WriterOptions,
    params: &crate::encrypt_setup::EncryptParams,
    existing_max: u32,
    metadata_ref: Option<ObjectRef>,
    id0: &[u8],
) -> Result<EncryptionContext> {
    use crate::encrypt_setup::EncryptMethod;
    use crate::security::standard::{
        build_v1_v2_encrypt_dict, build_v4_encrypt_dict, ObjectKeyAlg, V1V2EncryptParams,
        V4CryptMethod, V4EncryptParams,
    };

    let id0 = id0.to_vec();

    let (encrypt_dict, file_key, cipher, encryption_v, encryption_r) = match params.method {
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
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Aes), 4, 4)
        }
        EncryptMethod::V5R6Aes256 => {
            use crate::security::standard::{build_v5_r6_encrypt_dict, V5R6EncryptParams};
            // V=5 R=6 needs 68 bytes of fresh secret material (file key + four
            // 8-byte salts + 4-byte /Perms tail). Unlike V<5, /ID[0] does NOT
            // feed the key derivation — the file key is a standalone CSPRNG
            // value, so V=5 output is never byte-identical across runs.
            let secrets = generate_v5r6_secrets(options)?;
            let v5 = V5R6EncryptParams {
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                encrypt_metadata: params.encrypt_metadata,
            };
            let dict = build_v5_r6_encrypt_dict(&v5, &secrets)?;
            (
                dict,
                secrets.file_key.to_vec(),
                WriteCipher::FileKeyAes256,
                5,
                6,
            )
        }
        EncryptMethod::V5R5Aes256 => {
            use crate::security::standard::{build_v5_r5_encrypt_dict, V5R6EncryptParams};
            let secrets = generate_v5r6_secrets(options)?;
            let v5 = V5R6EncryptParams {
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                encrypt_metadata: params.encrypt_metadata,
            };
            let dict = build_v5_r5_encrypt_dict(&v5, &secrets)?;
            (
                dict,
                secrets.file_key.to_vec(),
                WriteCipher::FileKeyAes256,
                5,
                5,
            )
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
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4), 1, 2)
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
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4), 2, 3)
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
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4), 4, 4)
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
        encryption_v,
        encryption_r,
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
fn generate_v5r6_secrets(
    _options: &WriterOptions,
) -> Result<crate::security::standard::V5R6Secrets> {
    #[cfg(any(test, feature = "qpdf-zlib-compat"))]
    if let Some(randomness) = _options.v5_randomness {
        return Ok(crate::security::standard::V5R6Secrets {
            file_key: randomness.file_key,
            user_validation_salt: randomness.user_validation_salt,
            user_key_salt: randomness.user_key_salt,
            owner_validation_salt: randomness.owner_validation_salt,
            owner_key_salt: randomness.owner_key_salt,
            perms_random_tail: randomness.perms_random_tail,
        });
    }

    let mut buf = [0u8; 68];
    getrandom::fill(&mut buf).map_err(|e| {
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
/// (the `--copy-encryption` path or PdfWriter's source-preservation
/// path).
///
/// qpdf does not copy the donor dictionary byte-for-byte. It passes the
/// authenticated donor values through `setEncryptionParametersInternal`:
/// V<4 remains RC4, V4 is always rewritten to AESV2, and V5 is rewritten to
/// AESV3 while retaining the donor's recovered file key. Rebuild the same
/// canonical dictionary here so a V4 RC4 donor has the same observable result
/// as qpdf's copy path.
pub(crate) fn build_copy_encryption_context(
    src: &crate::encrypt_setup::CopyEncryptionSource,
    options: &WriterOptions,
    existing_max: u32,
    metadata_ref: Option<ObjectRef>,
) -> Result<EncryptionContext> {
    let (encrypt_dict, encryption_v, encryption_r, cipher) = canonical_copy_encryption(src)?;

    let encrypt_num = existing_max.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported(
            "full-rewrite copy-encryption: /Encrypt object number overflows u32".to_string(),
        )
    })?;

    let encrypt_metadata = copy_encryption_encrypts_metadata_from_dict(&encrypt_dict);

    Ok(EncryptionContext {
        encrypt_dict,
        file_key: src.file_key.clone(),
        cipher,
        encryption_v,
        encryption_r,
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
        id0: src.id0.clone(),
        static_aes_iv: options.static_aes_iv,
        encrypt_metadata,
        metadata_ref: if encrypt_metadata { None } else { metadata_ref },
    })
}

/// Rebuild the dictionary qpdf emits from `copyEncryptionParameters` and
/// select the corresponding object-key cipher.
fn canonical_copy_encryption(
    src: &crate::encrypt_setup::CopyEncryptionSource,
) -> Result<(Dictionary, i32, i32, WriteCipher)> {
    use crate::security::standard::ObjectKeyAlg;

    let version = copy_integer(&src.encrypt_dict, "V")?;
    let revision = copy_integer(&src.encrypt_dict, "R")?;
    let version_i32 = i32::try_from(version).map_err(|_| {
        crate::Error::Unsupported(format!("copy-encryption /V is out of range: {version}"))
    })?;
    let revision_i32 = i32::try_from(revision).map_err(|_| {
        crate::Error::Unsupported(format!("copy-encryption /R is out of range: {revision}"))
    })?;
    let length_bits = if version == 1 {
        40
    } else {
        copy_integer(&src.encrypt_dict, "Length")?
    };
    if !(40..=256).contains(&length_bits) || length_bits % 8 != 0 {
        return Err(crate::Error::Unsupported(format!(
            "copy-encryption /Length is invalid: {length_bits} bits"
        )));
    }

    let expected_key_len = if version >= 5 {
        if version != 5 || !matches!(revision, 5 | 6) || length_bits != 256 {
            return Err(crate::Error::Unsupported(format!(
                "unsupported copy-encryption Standard handler V={version} R={revision} Length={length_bits}"
            )));
        }
        32
    } else {
        if !matches!(version, 1 | 2 | 4)
            || (version == 1 && revision != 2)
            || (version == 2 && !matches!(revision, 2 | 3))
            || (version == 4 && revision != 4)
        {
            return Err(crate::Error::Unsupported(format!(
                "unsupported copy-encryption Standard handler V={version} R={revision}"
            )));
        }
        // cov:ignore-start: length_bits is range-checked and divisible by eight;
        // the supported targets can represent every resulting key length.
        usize::try_from(length_bits / 8).map_err(|_| {
            crate::Error::Unsupported("copy-encryption key length overflows usize".into())
        })?
        // cov:ignore-end
    };
    if src.file_key.len() != expected_key_len {
        return Err(crate::Error::Unsupported(format!(
            "copy-encryption V={version} R={revision} file key must be {expected_key_len} bytes; got {}",
            src.file_key.len()
        )));
    }

    let p = copy_integer(&src.encrypt_dict, "P")?;
    let o = copy_string(&src.encrypt_dict, "O")?;
    let u = copy_string(&src.encrypt_dict, "U")?;
    let encrypt_metadata = copy_encryption_encrypts_metadata_from_dict(&src.encrypt_dict);

    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"Standard".to_vec()));
    dict.insert("V", Object::Integer(version));
    dict.insert("Length", Object::Integer(length_bits));
    dict.insert("R", Object::Integer(revision));
    dict.insert("P", Object::Integer(p));
    dict.insert("O", Object::String(o));
    dict.insert("U", Object::String(u));

    let cipher = if version >= 5 {
        let oe = copy_string(&src.encrypt_dict, "OE")?;
        let ue = copy_string(&src.encrypt_dict, "UE")?;
        let perms = copy_string(&src.encrypt_dict, "Perms")?;
        dict.insert("OE", Object::String(oe));
        dict.insert("UE", Object::String(ue));
        dict.insert("Perms", Object::String(perms));
        insert_standard_crypt_filter(&mut dict, b"AESV3", 32);
        WriteCipher::FileKeyAes256
    } else if version == 4 {
        // QPDFWriter::copyEncryptionParameters explicitly enables AES for all
        // V>=4 donors, even when the source /CFM was /V2.
        insert_standard_crypt_filter(&mut dict, b"AESV2", 16);
        WriteCipher::PerObject(ObjectKeyAlg::Aes)
    } else {
        WriteCipher::PerObject(ObjectKeyAlg::Rc4)
    };

    if revision >= 4 && !encrypt_metadata {
        dict.insert("EncryptMetadata", Object::Boolean(false));
    }
    Ok((dict, version_i32, revision_i32, cipher))
}

fn copy_integer(dict: &Dictionary, key: &str) -> Result<i64> {
    dict.get(key).and_then(Object::as_integer).ok_or_else(|| {
        crate::Error::Unsupported(format!("copy-encryption /{key} must be an integer"))
    })
}

fn copy_string(dict: &Dictionary, key: &str) -> Result<Vec<u8>> {
    dict.get(key)
        .and_then(Object::as_string)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            crate::Error::Unsupported(format!("copy-encryption /{key} must be a string"))
        })
}

fn insert_standard_crypt_filter(dict: &mut Dictionary, cfm: &[u8], length: i64) {
    let mut std_cf = Dictionary::new();
    std_cf.insert("AuthEvent", Object::Name(b"DocOpen".to_vec()));
    std_cf.insert("CFM", Object::Name(cfm.to_vec()));
    std_cf.insert("Length", Object::Integer(length));
    let mut cf = Dictionary::new();
    cf.insert("StdCF", Object::Dictionary(std_cf));
    dict.insert("CF", Object::Dictionary(cf));
    dict.insert("StmF", Object::Name(b"StdCF".to_vec()));
    dict.insert("StrF", Object::Name(b"StdCF".to_vec()));
}

/// Return the donor's metadata-encryption policy using qpdf's default. qpdf
/// only changes its default when `/EncryptMetadata` is present and boolean;
/// an absent or otherwise unusable entry means metadata remains encrypted.
pub(crate) fn copy_encryption_encrypts_metadata(
    src: &crate::encrypt_setup::CopyEncryptionSource,
) -> bool {
    copy_encryption_encrypts_metadata_from_dict(&src.encrypt_dict)
}

fn copy_encryption_encrypts_metadata_from_dict(dict: &Dictionary) -> bool {
    dict.get("EncryptMetadata")
        .and_then(Object::as_bool)
        .unwrap_or(true)
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

/// Byte length of the serialized deterministic `/ID` array `[<id0_hex><id1_hex>]`
/// for an id0 of `id0_len` bytes: `[` + (`<` + 2*id0_len hex + `>`) + (`<` + 32 hex + `>`) + `]`.
pub(crate) const fn deterministic_id_array_len(id0_len: usize) -> usize {
    1 + (1 + 2 * id0_len + 1) + (1 + 32 + 1) + 1
}
/// The width for a 16-byte id0 (the common case): 70 bytes. Kept as a named
/// alias for the many length assertions in the linearization writer's tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const DETERMINISTIC_ID_ARRAY_LEN: usize = deterministic_id_array_len(16);

/// Serialize a deterministic `/ID` array as the fixed-width hex form qpdf emits:
/// `[<id0_hex><id1_hex>]`, with no inner spaces. The permanent identifier `id0`
/// may be any length (qpdf preserves a source `/ID[0]` verbatim regardless of
/// length); the changing identifier `id1` is always a 16-byte md5. The serialized
/// length is [`deterministic_id_array_len`]`(id0.len())`. Building the bytes by
/// hand (rather than via [`Object::write_pdf`]) guarantees the hex form even when
/// a digest happens to be all-printable, so the value is always the same fixed
/// width regardless of its bytes. The classic linearized writer calls this
/// directly to emit the final identifier at each `/ID` site in its last write
/// pass (qpdf's 2-pass scheme); the ObjStm linearized writer uses it for both the
/// all-zero placeholder and the patched-in final value, whose equal width leaves
/// every later byte offset intact. (The flat write paths instead direct-write the
/// final value via [`write_deterministic_id_inline`].)
pub(crate) fn write_deterministic_id_array(out: &mut Vec<u8>, id0: &[u8], id1: &[u8; 16]) {
    out.push(b'[');
    for id in [id0, &id1[..]] {
        out.push(b'<');
        push_hex_lower(out, id);
        out.push(b'>');
    }
    out.push(b']');
}

/// Extract the source trailer's non-empty string permanent identifier `/ID[0]`.
/// qpdf's `getOriginalID1` reads only that first array item: `/ID[1]` may be
/// absent or have any type. Returns `None` for a missing or non-array `/ID`, or
/// a missing, non-string, or empty first item, in which case qpdf reuses the
/// changing identifier as the permanent one. The returned bytes are preserved
/// verbatim at any length: qpdf copies `/ID[0]` unchanged and only regenerates
/// the 16-byte changing identifier `/ID[1]`, so the serialized `/ID` array is
/// [`deterministic_id_array_len`]`(id0.len())` bytes wide. An empty `/ID[0]` is
/// treated as absent (not preserved as `""`).
fn source_permanent_id_value(source_id: Option<&Object>) -> Option<Vec<u8>> {
    match source_id {
        Some(Object::Array(values)) => match values.first() {
            Some(Object::String(first)) if !first.is_empty() => Some(first.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn source_permanent_id(trailer: &Dictionary) -> Option<Vec<u8>> {
    source_permanent_id_value(trailer.get("ID"))
}

/// Generate qpdf's ordinary/static two-element `/ID` array.
///
/// qpdf creates the changing identifier once and then uses the non-empty
/// source permanent identifier when one is available. An absent or empty
/// source permanent identifier falls back to that same changing identifier,
/// so `/ID[0] == /ID[1]` for a first save or an empty source `/ID[0]`.
pub(crate) fn generate_id_array(source_id: Option<&Object>, static_id: bool) -> Object {
    let changing_id = if static_id {
        QPDF_STATIC_ID.to_vec()
    } else {
        fresh_id_bytes().to_vec()
    };
    let permanent_id = source_permanent_id_value(source_id).unwrap_or_else(|| changing_id.clone());
    Object::Array(vec![
        Object::String(permanent_id),
        Object::String(changing_id),
    ])
}

/// Generate qpdf's ordinary/static two-element `/ID` array as a canonical
/// handle, preserving the same source-permanent-id and changing-id rules as
/// [`generate_id_array`] without crossing back through `Object`.
pub(crate) fn generate_id_handle(source_id0: Option<&[u8]>, static_id: bool) -> ObjectHandle {
    let changing_id = if static_id {
        QPDF_STATIC_ID.to_vec()
    } else {
        fresh_id_bytes().to_vec()
    };
    let permanent_id = source_id0
        .filter(|id0| !id0.is_empty())
        .map(<[u8]>::to_vec)
        .unwrap_or_else(|| changing_id.clone());
    ObjectHandle::array(vec![
        ObjectHandle::string(permanent_id),
        ObjectHandle::string(changing_id),
    ])
}

/// Build the `/Info`-derived suffix of qpdf's deterministic `/ID` seed.
///
/// qpdf (`QPDFWriter::generateID`) appends, for every `/Info` entry whose value
/// is a string, `" "` followed by the string's *decoded* bytes, iterating keys
/// in sorted order (qpdf's `getKeys()` returns names sorted). Non-string
/// entries are skipped. The live `/Info` handle and each value may be an
/// indirect reference, so both are resolved (PDF allows any value to be
/// indirect, ISO 32000-1 §7.3.10). The returned bytes are appended after
/// `" QPDF "` to form the seed.
pub(crate) fn deterministic_id_info_suffix<R: Read + Seek>(pdf: &mut Pdf<R>) -> Vec<u8> {
    let trailer = pdf.trailer_handle();
    let info = match trailer.try_get_key(b"/Info") {
        Ok(info) => info,
        Err(_) => return Vec::new(), // cov:ignore: defensive resolver-error fallback
    };
    let dict = match info.try_as_dictionary() {
        Ok(Some(dict)) => dict,
        Ok(None) | Err(_) => return Vec::new(), // cov:ignore: defensive resolver-error fallback
    };
    // `ObjectHandle::try_as_dictionary` returns qpdf's lexicographically sorted
    // decoded names, matching `QPDFObjectHandle::getKeys()`.
    let mut suffix = Vec::new();
    for (_key, value) in dict {
        if value.try_dereference().is_err() {
            continue; // cov:ignore: defensive resolver-error fallback
        }
        if let Some(bytes) = value.as_string() {
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
/// 4. `/ID[0]` (permanent identifier) = `source_id0` (verbatim, any length) when
///    present, else a copy of `/ID[1]`.
pub(crate) fn compute_deterministic_id(
    bytes: &[u8],
    id_array_offset: usize,
    info_suffix: &[u8],
    source_id0: Option<&[u8]>,
) -> (Vec<u8>, [u8; 16]) {
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
    let id0 = source_id0
        .map(<[u8]>::to_vec)
        .unwrap_or_else(|| id1.to_vec());
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
    source_id0: Option<&[u8]>,
) {
    out.push(b'[');
    let id_array_offset = out.len() - 1; // index of the just-pushed `[`
    let (id0, id1) = compute_deterministic_id(out, id_array_offset, info_suffix, source_id0);
    for id in [id0.as_slice(), &id1[..]] {
        out.push(b'<');
        push_hex_lower(out, id);
        out.push(b'>');
    }
    out.push(b']');
}

fn generated_id_handle(value: &Object) -> Result<ObjectHandle> {
    let Object::Array(values) = value else {
        // cov:ignore-start: generate_id_array constructs the validated two-string shape before this boundary
        return Err(crate::Error::Unsupported(
            "writer generated /ID is not an array".to_string(),
        ));
        // cov:ignore-end
    };
    let [Object::String(id0), Object::String(id1)] = values.as_slice() else {
        // cov:ignore-start: generate_id_array constructs exactly two string elements before this boundary
        return Err(crate::Error::Unsupported(
            "writer generated /ID does not contain two strings".to_string(),
        ));
        // cov:ignore-end
    };
    Ok(ObjectHandle::array(vec![
        ObjectHandle::string(id0.clone()),
        ObjectHandle::string(id1.clone()),
    ]))
}

/// Apply writer-owned trailer values without converting the live trailer back
/// through the legacy `Dictionary` bridge. `/Root` and `/Encrypt` are already
/// output-space references, while `/ID` is a direct writer-owned array.
fn apply_encrypt_trailer_handle_entries<R: Read + Seek>(
    trailer: &ObjectHandle,
    pdf: &Pdf<R>,
    options: &WriterOptions,
    encrypt_ctx: Option<&EncryptionContext>,
    deterministic_id: bool,
    generated_id: Option<&ObjectHandle>,
) -> Result<()> {
    if let Some(ctx) = encrypt_ctx {
        trailer.replace_key(
            b"/Encrypt",
            ObjectHandle::from_value(ObjectValue::Reference(ctx.encrypt_ref)),
        )?; // cov:ignore: validated trailer mutation; LLVM attributes this continuation to the call setup
        if let Some(id) = generated_id {
            trailer.replace_key(b"/ID", id.shallow_copy()?)?;
        } else {
            let id1 = if options.static_id {
                QPDF_STATIC_ID.to_vec()
            } else {
                fresh_id_bytes().to_vec()
            };
            trailer.replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(ctx.id0.clone()),
                    ObjectHandle::string(id1),
                ]),
            )?; // cov:ignore: validated trailer mutation; LLVM attributes this continuation to the call setup
        }
    } else {
        if pdf.is_encrypted() {
            trailer.remove_key(b"/Encrypt");
        }
        if deterministic_id {
            trailer.replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(vec![0; 16]),
                    ObjectHandle::string(vec![0; 16]),
                ]),
            )?; // cov:ignore: validated deterministic-ID trailer mutation; LLVM attributes this continuation to the call setup
        } else if let Some(id) = generated_id {
            trailer.replace_key(b"/ID", id.shallow_copy()?)?;
        } else {
            // cov:ignore-start: generated_id_handle is required before this non-encrypted trailer path
            return Err(crate::Error::Unsupported(
                "writer trailer is missing its generated /ID".to_string(),
            ));
            // cov:ignore-end
        }
    }
    Ok(())
}

/// Build the trimmed trailer shell used by the non-linearized full rewrite.
/// The source trailer and all surviving child values stay in the canonical
/// ObjectHandle graph; only writer-owned structural values are replaced.
#[allow(clippy::too_many_arguments)] // qpdf keeps source form, output form, ID, and encryption independent
fn build_writer_trailer_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source_xref_stream: bool,
    output_xref_stream: bool,
    size: usize,
    root: ObjectRef,
    options: &WriterOptions,
    encrypt_ctx: Option<&EncryptionContext>,
    deterministic_id: bool,
    generated_id: Option<&ObjectHandle>,
) -> Result<ObjectHandle> {
    let trailer = pdf.trailer_handle().shallow_copy()?;
    for key in [b"/ID".as_slice(), b"/Encrypt", b"/Prev"] {
        trailer.remove_key(key);
    }
    if source_xref_stream || output_xref_stream {
        for key in [
            b"/Type".as_slice(),
            b"/F",
            b"/FFilter",
            b"/FDecodeParms",
            b"/W",
            b"/Index",
            b"/Length",
            b"/Filter",
            b"/DecodeParms",
            b"/XRefStm",
        ] {
            trailer.remove_key(key);
        }
    }
    trailer.replace_key(
        b"/Size",
        ObjectHandle::integer(i64::try_from(size).map_err(|_| {
            // cov:ignore-start: supported writer object counts fit in i64
            crate::Error::Unsupported("writer trailer /Size does not fit in i64".to_string())
            // cov:ignore-end
        })?), // cov:ignore: supported writer object counts fit in i64
    )?; // cov:ignore: validated /Size replacement; LLVM attributes this continuation to the call setup
    trailer.replace_key(
        b"/Root",
        ObjectHandle::from_value(ObjectValue::Reference(root)),
    )?; // cov:ignore: validated /Root replacement; LLVM attributes this continuation to the call setup
    apply_encrypt_trailer_handle_entries(
        &trailer,
        pdf,
        options,
        encrypt_ctx,
        deterministic_id,
        generated_id,
    )?; // cov:ignore: validated writer-owned trailer entries; LLVM attributes this continuation to the call setup
    Ok(trailer)
}

/// Whether `cipher` needs an AES CBC initialization vector: `true` for both
/// AES variants (V=4 AESV2 `PerObject(Aes)` and V=5 AESV3 `FileKeyAes256`),
/// `false` for RC4 (a stream cipher with no IV concept).
///
/// Shared by the canonical encrypted-string and stream pipeline stages and
/// `crate::linearization::writer::write_linearized` (which draws the hint
/// stream's single per-invocation IV under the same condition).
pub(crate) fn cipher_needs_aes_iv(cipher: WriteCipher) -> bool {
    use crate::security::standard::ObjectKeyAlg;
    matches!(
        cipher,
        WriteCipher::PerObject(ObjectKeyAlg::Aes) | WriteCipher::FileKeyAes256
    )
}

/// Apply qpdf's `QPDFWriter::adjustAESStreamLength` rule before a stream
/// dictionary is unparsed (`libqpdf/QPDFWriter.cc:965-973`).
fn writer_has_current_data_key(ctx: &EncryptionContext) -> bool {
    match ctx.cipher {
        WriteCipher::PerObject(_) => true,
        WriteCipher::FileKeyAes256 => !ctx.file_key.is_empty(),
    }
}

pub(crate) fn adjust_aes_stream_length(
    length: &mut usize,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
) -> Result<()> {
    if encrypt_stream && writer_has_current_data_key(ctx) && cipher_needs_aes_iv(ctx.cipher) {
        let padding = 32 - (*length & 0xf);
        *length = (*length).checked_add(padding).ok_or_else(|| {
            // cov:ignore-start: allocating a Vec large enough to overflow usize is infeasible.
            crate::Error::Unsupported("encrypted stream /Length overflows usize".to_string())
            // cov:ignore-end
        })?; // cov:ignore: llvm-cov attributes this continuation to the unreachable overflow arm.
    }
    Ok(())
}

/// Finish a writer pipeline even when its write phase fails. qpdf's
/// `PipelinePopper` calls `finish` from its destructor before it restores the
/// previous stack frame (`libqpdf/QPDFWriter.cc:925-963`).
fn run_writer_pipeline(pipeline: &mut dyn Pipeline, data: &[u8]) -> Result<()> {
    let write_result = pipeline.write(data);
    let finish_result = pipeline.finish();
    if let Err(error) = write_result {
        return Err(error.into());
    }
    finish_result.map_err(Into::into)
}

/// Feed one emitted stream through qpdf's conditional encryption stage and
/// write it directly to the final output sink. The `Count` stage preserves
/// qpdf's last-byte framing decision while `PlString` is the output sink.
fn pipe_writer_stream_payload(
    out: &mut Vec<u8>,
    data: &[u8],
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<u8> {
    let mut sink = PlString::new("writer stream output", None, out);
    let mut count = crate::pipeline::count::Count::new("writer stream count", &mut sink);
    let explicit_iv = explicit_iv.or_else(|| {
        (ctx.static_aes_iv && cipher_needs_aes_iv(ctx.cipher))
            .then(crate::pipeline::aes::static_initialization_vector)
    });

    if !encrypt_stream {
        run_writer_pipeline(&mut count, data)?;
        return Ok(count.last_byte());
    }

    let mut state = encryption_state::WriterEncryptionState::new(
        true,
        ctx.file_key.clone(),
        cipher_needs_aes_iv(ctx.cipher),
        ctx.encryption_v,
        ctx.encryption_r,
    );
    state.with_object_data_key(object_ref.number, None, |state| {
        let key = state.current_data_key().ok_or_else(|| {
            // cov:ignore-start: with_object_data_key always installs a data key before this closure.
            crate::Error::Internal(
                "QPDFWriter stream encryption data key was not initialized".to_string(),
            )
            // cov:ignore-end
        })?; // cov:ignore: llvm-cov attributes this continuation to the impossible error arm.
        if key.is_empty() {
            run_writer_pipeline(&mut count, data)?;
            return Ok(());
        }

        match ctx.cipher {
            WriteCipher::PerObject(crate::security::standard::ObjectKeyAlg::Rc4) => {
                let mut stage =
                    crate::pipeline::rc4::PlRc4::new("rc4 stream encryption", &mut count, key)?;
                run_writer_pipeline(&mut stage, data)
            }
            WriteCipher::PerObject(crate::security::standard::ObjectKeyAlg::Aes)
            | WriteCipher::FileKeyAes256 => {
                if let Some(iv) = explicit_iv {
                    count.write(&iv)?;
                    let mut stage = crate::pipeline::aes::PlAesPdf::new_encrypt(
                        "aes stream encryption",
                        &mut count,
                        key,
                    )?;
                    stage.set_iv(&iv)?;
                    run_writer_pipeline(&mut stage, data)
                } else {
                    let mut stage = crate::pipeline::aes::PlAesPdf::new_encrypt(
                        "aes stream encryption",
                        &mut count,
                        key,
                    )?; // cov:ignore: the no-explicit-IV AES route executes; this call continuation has no counter.
                    run_writer_pipeline(&mut stage, data)
                }
            }
        }
    })?;

    Ok(count.last_byte())
}

/// Write a stream payload through the qpdf-shaped writer pipeline, including
/// the final `endstream` framing decision based on the pipeline's last byte.
pub(crate) fn write_stream_payload_with_pipeline(
    out: &mut Vec<u8>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<bool> {
    out.extend_from_slice(b"\nstream\n");
    let last_byte =
        pipe_writer_stream_payload(out, data, object_ref, ctx, encrypt_stream, explicit_iv)?;
    let add_newline = match policy {
        NewlineBeforeEndstream::Yes => true,
        NewlineBeforeEndstream::No => last_byte != b'\n',
        NewlineBeforeEndstream::Never => false,
    };
    if add_newline {
        out.push(b'\n');
    }
    out.extend_from_slice(b"endstream");
    Ok(add_newline)
}

/// Re-encode a resolved stream object per the effective compression policy,
/// returning the re-encoded object and whether the **source** filter chain was
/// already a lone `/FlateDecode`.
///
/// This is the byte-critical choke point shared by the legacy excluded-mode
/// full-rewrite path, the plain body writer, and the linearized body writer.
/// Keeping it in one place prevents those paths from drifting on qpdf's
/// recovered-stream framing or re-filter rules:
///
/// * `recovered_stream_eol`, when present, is restored exactly once before any
///   preserve/decode/re-encode decision. The reader records it only while an
///   `endstream` scan remains authoritative.
/// * `CompressStreams::Yes` on an already-lone-`/FlateDecode` source (and no
///   `/F` external-data entry, no `--recompress-flate`, no content
///   normalization, and `is_data_modified` false) is **preserved verbatim**
///   — qpdf does not decode + re-encode it — with `/Length` normalized to the
///   raw data length.
/// * any other `Yes`/`No` policy decodes and re-encodes via
///   [`apply_stream_compress_policy`].
/// * preserve mode (`None`) passes the dict + raw bytes through unchanged.
///
/// `is_data_modified` mirrors `QPDFObjectHandle::isDataModified()`
/// (`QPDF_Stream.cc:321-324`), which `QPDFWriter::willFilterStream`
/// consults before the lone-`/FlateDecode` exemption
/// (`QPDFWriter.cc:1234-1245`): a stream carrying a registered token filter
/// is never eligible for the verbatim shortcut, even though this
/// materialized-`Object` caller has no token-filter machinery of its own and
/// so always decodes + re-encodes the **already-materialized** bytes rather
/// than running the filter. Callers that never observe token-filtered
/// streams (`write_pclm`) may pass `false` unconditionally.
///
/// The returned bool feeds [`write_reencoded_object`], which only appends a
/// regenerated `/Filter` (qpdf's re-filtered key order) when the source was NOT
/// already a lone `/FlateDecode`.
pub(crate) fn reencode_stream_for_compress(
    mut stream: crate::Stream,
    options: &WriterOptions,
    is_data_modified: bool,
    qpdf_plain_empty_refilter: bool,
    recovered_stream_eol: Option<&[u8]>,
    normalize_content: bool,
    apply_full_rewrite_metadata_policy: bool,
) -> (Object, bool) {
    if let Some(eol) = recovered_stream_eol {
        stream.data.extend_from_slice(eol);
    }
    // qpdf's writer always writes cleartext `/Type /Metadata` streams
    // through the uncompress path (QPDFWriter.cc:1251-1281), even when the
    // global writer requested compression or stream-data preservation. This
    // is PdfWriter's standard full-rewrite policy, not the shared helper's
    // plain or linearized route policy; those callers opt out explicitly.
    // Metadata is not page content, so it must never receive content-token
    // normalization. The encryption layer applies any requested payload
    // encryption after this decision; the cleartext metadata exemption is
    // handled there.
    let metadata_is_cleartext = options
        .encrypt
        .as_ref()
        .is_none_or(|params| !params.encrypt_metadata)
        && options
            .copy_encryption
            .as_ref()
            .is_none_or(|source| !copy_encryption_encrypts_metadata(source));
    let is_metadata_stream = apply_full_rewrite_metadata_policy
        && metadata_is_cleartext
        && matches!(
            stream.dict.get("Type"),
            Some(Object::Name(name)) if name.as_slice() == b"Metadata"
        );
    let stream_policy = if is_metadata_stream {
        Some(CompressStreams::No)
    } else {
        effective_stream_policy(options)
    };
    let stream_decode_level = if is_metadata_stream {
        DecodeLevel::All
    } else {
        options.decode_level
    };
    let stream_normalization = normalize_content && !is_metadata_stream;
    let source_filter_is_lone_flate = is_lone_flate(stream.dict.get("Filter"));
    let mut reencoded = match stream_policy {
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
        //
        // `!is_data_modified` mirrors `willFilterStream`'s own exemption guard
        // (`QPDFWriter.cc:1234-1245`): a token-filtered stream is never
        // eligible for the verbatim shortcut, so it falls through to the
        // re-encode arm below like any other non-lone-Flate source.
        Some(CompressStreams::Yes)
            if source_filter_is_lone_flate
                && !is_data_modified
                && !options.recompress_flate
                && !stream_normalization
                && !is_metadata_stream
                && stream.dict.get("F").is_none() =>
        {
            let mut stream = stream;
            let len = i64::try_from(stream.data.len()).unwrap_or(i64::MAX);
            stream.dict.insert("Length", Object::Integer(len));
            Object::Stream(stream)
        }
        Some(compress_policy) => apply_stream_compress_policy_with_decode_level(
            &stream,
            compress_policy,
            stream_decode_level,
            stream_normalization,
        ),
        // Preserve mode: keep the raw bytes verbatim (no decode/re-encode), but
        // still normalize /Length to a direct integer of the raw byte count.
        // qpdf direct-izes EVERY stream's /Length even under --stream-data=preserve
        // (flpdf-3g8o); a source may carry an indirect /Length. This applies
        // regardless of whether the holder then orphans (and is dropped by the
        // caller's reachability GC) or stays live because something else also
        // references it — the dict entry written here is always direct.
        None => {
            let mut stream = stream;
            let len = i64::try_from(stream.data.len()).unwrap_or(i64::MAX);
            stream.dict.insert("Length", Object::Integer(len));
            Object::Stream(stream)
        }
    };
    if qpdf_plain_empty_refilter
        && !source_filter_is_lone_flate
        && !is_metadata_stream
        && matches!(stream_policy, Some(CompressStreams::Yes))
    {
        let reencoded_stream = reencoded
            .as_stream_mut()
            .expect("stream compression always returns a stream");
        if filters::decode_stream_data(&reencoded_stream.dict, &reencoded_stream.data)
            .is_ok_and(|decoded| decoded.is_empty())
        {
            reencoded_stream.data.clear();
            reencoded_stream
                .dict
                .insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            reencoded_stream.dict.insert("Length", Object::Integer(0));
        }
    }
    (reencoded, source_filter_is_lone_flate)
}

/// Append a re-encoded object's body to `bytes` in qpdf's **non-qdf**
/// serialization order. For a stream, a regenerated lone `/Filter /FlateDecode`
/// is emitted last (and `/Length` first) only when the compress policy
/// re-encoded a source that was NOT already a lone `/FlateDecode`
/// (`write_stream_to_buf_qpdf_order`); an already-Flate or preserved source keeps
/// its lexicographic order with `/Length` last. Non-stream objects serialize
/// normally. Shared by the legacy excluded-mode writer and the plain pipeline.
pub(crate) struct StreamEncryptionOptions<'a> {
    context: Option<&'a EncryptionContext>,
    encrypt_strings: bool,
}

impl<'a> StreamEncryptionOptions<'a> {
    pub(crate) const fn new(context: Option<&'a EncryptionContext>, encrypt_strings: bool) -> Self {
        Self {
            context,
            encrypt_strings,
        }
    }
}

fn write_reencoded_object(
    bytes: &mut Vec<u8>,
    reencoded: &Object,
    source_filter_is_lone_flate: bool,
    options: &WriterOptions,
    encrypted_strings: Option<&mut encrypted_strings::EncryptedStringEmitter>,
    emitted_ref: ObjectRef,
    stream_encryption: StreamEncryptionOptions<'_>,
) -> Result<()> {
    match reencoded {
        Object::Stream(s) => {
            let refiltered = matches!(effective_stream_policy(options), Some(CompressStreams::Yes))
                && !source_filter_is_lone_flate
                && is_lone_flate(s.dict.get("Filter"));
            if let Some(ctx) = stream_encryption.context {
                let mut dict = s.dict.clone();
                let mut stream_length = s.data.len();
                adjust_aes_stream_length(
                    &mut stream_length,
                    ctx,
                    stream_encryption.encrypt_strings,
                )?; // cov:ignore: covered route; llvm-cov has no counter for this multiline argument.
                dict.insert(
                    "Length",
                    Object::Integer(i64::try_from(stream_length).map_err(|_| {
                        // cov:ignore-start: an allocatable stream length cannot exceed i64::MAX.
                        crate::Error::Unsupported(
                            "encrypted stream /Length does not fit in i64".to_string(),
                        )
                        // cov:ignore-end
                    })?), // cov:ignore: llvm-cov attributes this continuation to the impossible overflow arm.
                );
                if let Some(emitter) = encrypted_strings {
                    emitter.write_stream_dict(
                        bytes,
                        emitted_ref,
                        None,
                        &dict,
                        encrypted_strings::StreamDictOptions::new(
                            false,
                            refiltered,
                            stream_encryption.encrypt_strings,
                        ),
                    )?; // cov:ignore: the encrypted-dictionary route executes; this call continuation has no counter.
                } else {
                    dict.write_pdf_stream(bytes, refiltered);
                }
                write_stream_payload_with_pipeline(
                    bytes,
                    &s.data,
                    options.newline_before_endstream,
                    emitted_ref,
                    ctx,
                    stream_encryption.encrypt_strings,
                    None,
                )?; // cov:ignore: the encrypted-payload route executes; this call continuation has no counter.
            } else if let Some(emitter) = encrypted_strings {
                emitter.write_stream_dict(
                    bytes,
                    emitted_ref,
                    None,
                    &s.dict,
                    encrypted_strings::StreamDictOptions::new(false, refiltered, true),
                )?; // cov:ignore: the dictionary-only route executes; this call continuation has no counter.
                write_stream_payload(bytes, &s.data, options.newline_before_endstream);
            } else {
                s.dict.write_pdf_stream(bytes, refiltered);
                write_stream_payload(bytes, &s.data, options.newline_before_endstream);
            }
        }
        // cov:ignore-start: unreachable — callers only pass stream objects and
        // reencode_stream_for_compress always returns Object::Stream.
        other => {
            if let Some(emitter) = encrypted_strings {
                emitter.write_object(bytes, emitted_ref, None, other, false)?;
            } else {
                other.write_pdf(bytes);
            }
        } // cov:ignore-end
    }
    Ok(())
}

pub(crate) fn emit_canonical_pdf<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    // Snapshot the source Catalog AND its dirty-flag state BEFORE any ADBE
    // injection / strip mutates them, so those output-only mutations do not
    // leak into the caller's Pdf handle. Restored below regardless of whether
    // the write succeeds — the Pdf handle is safe to reuse for subsequent
    // writes (or for read APIs like page enumeration) after this call
    // returns. The dirty flag is captured too because `Pdf::set_object` used
    // for the restore unconditionally marks its target dirty; without the
    // dirty-flag restore a subsequent `write_pdf` incremental append would
    // spuriously emit a Catalog delta.
    let catalog_snapshot = pdf.root_ref().and_then(|r| {
        let was_dirty = pdf.is_dirty(r);
        pdf.resolve(r).ok().map(|catalog| (r, catalog, was_dirty))
    });

    // QDF form (`qpdf --qdf`) is designed for human editing and requires a
    // line-anchored `endstream`, so the caller's `Never` policy — which
    // would place `endstream` immediately after the raw payload byte —
    // is incompatible with QDF. Promote `Never` to `No` here (qpdf's `No`
    // still emits a newline unless the payload's last byte is exactly
    // `\n`). Explicit `Yes` and `No` pass through unchanged so callers can
    // request the exact qpdf semantics via WriterOptions or the CLI
    // `--newline-before-endstream` flag.
    //
    // Mirrors qpdf QPDFWriter.cc:1560:
    //     `if (newline_before_endstream || (qdf_mode && last_char != '\n'))`
    // i.e. Yes → always add; QDF + non-'\n' end → add.
    let effective;
    let options =
        if options.qdf && options.newline_before_endstream == NewlineBeforeEndstream::Never {
            effective = WriterOptions {
                newline_before_endstream: NewlineBeforeEndstream::No,
                ..options.clone()
            };
            &effective
        } else {
            options
        };

    // qpdf's full-rewrite writer resolves source streams with its default
    // stream-framing recovery enabled. Keep that recovery scoped to the
    // specialized coordinator as well as the plain writer: this route now
    // resolves ordinary objects through ObjectHandle, so malformed source
    // `/Length` values must reach the same recovered stream shape before the
    // handle-aware consumer serializes them.
    let result =
        pdf.with_plain_writer_stream_recovery(|pdf| emit_canonical_pdf_inner(pdf, out, options));

    // Restore the original Catalog and its pre-write dirty state. Runs on
    // success and on error alike so partial injection state cannot leak
    // either. `set_object` marks the ref dirty; if the ref was clean before
    // this call, clear the dirty flag to leave the caller's Pdf byte-for-byte
    // equivalent to its pre-call state.
    // cov:ignore-start: outer if-let and inner-if closing braces are
    // llvm-cov region artifacts; the interior is exercised by
    // emit_canonical_pdf_does_not_leave_root_dirty_flag_set and
    // emit_canonical_pdf_preserves_pre_existing_root_dirty_flag.
    if let Some((root_ref, original, was_dirty)) = catalog_snapshot {
        pdf.set_object(root_ref, original);
        if !was_dirty {
            pdf.clear_dirty(root_ref);
        }
    }
    // cov:ignore-end

    result
}

/// Emit qpdf's page-oriented PCLm queue.
///
/// PCLm is intentionally kept out of the ordinary plain pipeline: qpdf
/// reserves numbers in a different order and inserts one synthetic transform
/// stream after every page-strip image. The output is otherwise a normal
/// full rewrite with a classic xref table and the usual trailer/ID handling.
fn write_pclm<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    // cov:ignore-start: emit_canonical_pdf_inner validates this combination
    // before dispatching to the private PCLm emitter.
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }
    // cov:ignore-end

    let plan = pclm::Plan::build(pdf)?;
    let version = effective_pdf_version(pdf.version(), options, false, false);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n%PCLm 1.0\n").as_bytes());
    bytes.extend_from_slice(options.extra_header_text.as_bytes());
    if !options.extra_header_text.is_empty() && !options.extra_header_text.ends_with('\n') {
        bytes.push(b'\n');
    }

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();
    let mut emitted_old_to_new = BTreeMap::<ObjectRef, ObjectRef>::new();

    for item in &plan.items {
        match *item {
            pclm::Item::Source { source, output } => {
                let mut object = pdf.resolve(source)?;
                // cov:ignore-start: the PCLm plan is built from the same validated
                // reference graph used for this rewrite; malformed remap input is rejected upstream.
                crate::rewrite_renumber::renumber_qpdf_refs_in_place(
                    pdf,
                    &mut object,
                    &plan.old_to_new,
                )?;
                // cov:ignore-end
                let offset = bytes.len();
                bytes.extend_from_slice(format!("{} 0 obj\n", output.number).as_bytes());
                match object {
                    Object::Stream(stream) => {
                        // PCLm image-strip pages never carry a registered
                        // token filter (AcroForm appearance regeneration is
                        // the only producer), so `is_data_modified` is always
                        // false here.
                        let (reencoded, source_filter_is_lone_flate) = reencode_stream_for_compress(
                            stream,
                            options,
                            false,
                            true,
                            pdf.recovered_stream_eol(source),
                            false,
                            false,
                        );
                        // cov:ignore-start: PCLm supplies a Vec sink and no encrypted
                        // string emitter, so this in-memory serializer has no error edge.
                        write_reencoded_object(
                            &mut bytes,
                            &reencoded,
                            source_filter_is_lone_flate,
                            options,
                            None,
                            output,
                            StreamEncryptionOptions::new(None, true),
                        )?;
                        // cov:ignore-end
                    }
                    other => other.write_pdf(&mut bytes),
                }
                bytes.extend_from_slice(b"\nendobj\n");
                offsets.insert(output.number, (0, offset));
                emitted_old_to_new.insert(source, output);
            }
            pclm::Item::Synthetic { output } => {
                let payload = b"q /image Do Q\n".to_vec();
                let mut dict = Dictionary::new();
                dict.insert("Length", Object::Integer(payload.len() as i64));
                let stream = crate::Stream::new(dict, payload);
                let offset = bytes.len();
                bytes.extend_from_slice(format!("{} 0 obj\n", output.number).as_bytes());
                write_stream_to_buf(&mut bytes, &stream, options.newline_before_endstream);
                bytes.extend_from_slice(b"\nendobj\n");
                offsets.insert(output.number, (0, offset));
            }
        }
        report_progress_event(options);
    }

    let max_object_number = offsets.keys().next_back().copied().unwrap_or(0);
    // cov:ignore-start: PCLm assigns contiguous u32 output numbers and supported
    // targets can represent the resulting object count in usize.
    let object_count = usize::try_from(max_object_number)
        .ok()
        .and_then(|number| number.checked_add(1))
        .ok_or_else(|| {
            crate::Error::Unsupported("PCLm object count does not fit in usize".to_string())
        })?;
    // cov:ignore-end
    let mut written_xref = BTreeMap::<ObjectRef, XrefEntry>::new();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {object_count}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..object_count {
        match offsets.get(&(number as u32)) {
            Some((generation, offset)) => {
                bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
                written_xref.insert(
                    ObjectRef::new(number as u32, 0),
                    XrefEntry::Uncompressed {
                        // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                        // on every supported target.
                        offset: u64::try_from(*offset).map_err(|_| {
                            crate::Error::Unsupported("PCLm xref offset does not fit u64".into())
                        })?,
                        // cov:ignore-end
                    },
                );
            }
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"), // cov:ignore: every PCLm item receives the next contiguous output number
        }
    }

    let mut trailer = pdf.trailer().clone();
    strip_writer_trailer_history_keys(&mut trailer);
    trailer.remove("Encrypt");
    let removed: BTreeSet<_> = pdf.deleted_object_refs().into_iter().collect();
    remap_qpdf_trailer_refs_with_removed(pdf, &mut trailer, &plan.old_to_new, &removed)?;
    trailer.insert("Size", Object::Integer(object_count as i64));
    trailer.insert("Root", Object::Reference(plan.root));
    let generated_id = if options.deterministic_id {
        None
    } else {
        Some(generate_id_array(
            pdf.trailer().get("ID"),
            options.static_id,
        ))
    };
    if options.deterministic_id {
        apply_deterministic_id_placeholder(&mut trailer);
    } else if let Some(id) = generated_id {
        trailer.insert("ID", id);
    }

    bytes.extend_from_slice(b"trailer ");
    if options.deterministic_id {
        let source_id0 = source_permanent_id(pdf.trailer());
        let info_suffix = deterministic_id_info_suffix(pdf);
        let mut id_writer = |out: &mut Vec<u8>| {
            write_deterministic_id_inline(out, &info_suffix, source_id0.as_deref())
        };
        trailer.write_pdf_trailer(&mut bytes, Some(&mut id_writer));
    } else {
        trailer.write_pdf_trailer(&mut bytes, None);
    }
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out.write_all(&bytes)?;
    Ok(WriterResult::new(emitted_old_to_new, written_xref))
}

fn emit_canonical_pdf_inner<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }

    // A forced sub-1.5 header suppresses object-stream generation: object
    // streams are a PDF 1.5 feature and qpdf will not emit them under a forced
    // version it must not exceed (observed on qpdf 11.9.0; `--object-streams=generate
    // --force-version=1.4` is byte-identical to `--object-streams=disable
    // --force-version=1.4`). Normalize to Disable here, before the routing
    // below, so the Generate path is skipped, the planner produces no batches,
    // AND an inherited source ObjStm is dropped; the xref-form override further
    // below then rebuilds a classic table even from an xref-stream source. All
    // three modes collapse to the identical classic output, matching qpdf (whose
    // preserve/disable/generate are byte-identical under a forced sub-1.5 header).
    //
    // Generate is normalized for any encryption state (the shipped behaviour).
    // Preserve is normalized only for the non-encrypted paths: `force<1.5 +
    // encrypt` is contradictory — encryption forces its own >=1.5 floor below,
    // so it never produces sub-1.5 output — and the encrypted ObjStm handling is
    // left byte-for-byte unchanged.
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();
    let requested_object_streams = options.object_streams;
    let suppressed_options;
    let options = if force_version_below_1_5(options)
        && (matches!(options.object_streams, ObjectStreamMode::Generate)
            || (!encrypting && matches!(options.object_streams, ObjectStreamMode::Preserve)))
    {
        suppressed_options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..options.clone()
        };
        &suppressed_options
    } else {
        options
    };

    // Run every remaining library-level option preflight before the specialized
    // Generate or Preserve emitters below can return early.
    if options.encrypt.is_some() && options.copy_encryption.is_some() {
        return Err(crate::Error::Unsupported(
            "encrypt and copy_encryption are mutually exclusive".to_string(),
        ));
    }
    if options.deterministic_id && encrypting {
        return Err(crate::Error::Unsupported(
            "the deterministic-id option is incompatible with encrypted output files".to_string(),
        ));
    }
    // flpdf-9hc.16.8: propagate the Adobe extension level into the destination
    // Catalog BEFORE any downstream dispatch, so every full-rewrite route sees
    // the injected Catalog.
    // When WriterOptions::min_extension_level requests an ext >= 1 (or the
    // source Catalog already carries one that survives the pairwise rule)
    // inject
    //   /Extensions << /ADBE << /BaseVersion /<ver> /ExtensionLevel <lvl> >> >>
    // so it becomes part of the Catalog the selected writer sees. A source
    // indirect /Extensions ref, if any, is inlined here and
    // drops out of the reachable graph — mirroring qpdf's writer behaviour.
    {
        let source_ver = pdf.version().to_string();
        let source_ext = pdf.adobe_extension_level().unwrap_or(0);
        // Predict whether the header floor will bump to PDF 1.5 due to
        // ObjStm emission, so the pairwise pairwise-contribution logic in
        // `effective_pdf_version_and_ext` sees the same version race that
        // the header writer will apply. Generate mode always emits ObjStm
        // through either the shared plain pipeline or this legacy excluded-mode
        // planner. `--qdf` forces ObjStm off. `Preserve` and `Disable` skip the floor here;
        // Preserve+source-has-ObjStm remains a latent edge case (walking
        // the source for eligibility would be expensive).
        let will_emit_objstm =
            !options.qdf && matches!(options.object_streams, ObjectStreamMode::Generate);
        let (eff_ver, eff_ext) = effective_pdf_version_and_ext(
            &source_ver,
            source_ext,
            options,
            false,
            will_emit_objstm,
        );
        if eff_ext > 0 {
            inject_adbe_extension(pdf, eff_ver, eff_ext)?;
        } else if catalog_has_extensions_adbe(pdf)? {
            // qpdf QPDFWriter.cc L1387/L1408/L1432: when the effective extension
            // level is 0, any `/Extensions /ADBE` key must be removed —
            // whether from a prior injection that lost the pairwise version race
            // (min_version bump / ObjStm floor drops the ext to 0) or from a
            // stale/malformed source /ADBE without a valid /ExtensionLevel.
            // strip_adbe_extension handles both branches: it drops /Extensions
            // when nothing else remains, otherwise keeps it with the non-ADBE
            // developer prefixes intact.
            strip_adbe_extension(pdf)?;
        }
    }

    if options.pclm {
        return write_pclm(pdf, out, options);
    }

    if plain::eligible(pdf.is_encrypted(), options, requested_object_streams) {
        return plain::write_plain(pdf, out, options);
    }

    // Only specialized modes reach the legacy coordinator below: QDF, output or
    // copied encryption, source-encrypted input, and requested Preserve/Generate
    // suppressed to Disable by a forced version below 1.5. Its container planner
    // and generic xref emitter remain live for those explicitly excluded modes.
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    // Catalog-first renumber (flpdf-9hc.32): assign output object numbers in
    // qpdf's `enqueueObjectsStandard` BFS order so that plain rewrite output is
    // byte-identical to `qpdf --static-id`. `build` borrows `pdf` mutably (lazy
    // load) and returns an owned map, releasing the borrow before the loop.
    //
    // Always use `skip_length = true`: in QDF mode the holder objects are
    // freshly assigned sequential emission numbers by the pre-scan below (not
    // reused from the source), so a prior-QDF-pass holder reachable only via a
    // /Length edge is NOT numbered here and disappears cleanly from `renumbered`.
    // In non-QDF mode this is the same behaviour as before.
    use crate::rewrite_renumber::CanonicalCatalogFirstRenumber;
    // The unencrypted/non-QDF case can reach this legacy path only when a
    // requested Preserve or Generate mode was suppressed to Disable by
    // force-version < 1.5. Keep its qpdf null-visibility behavior byte-stable.
    let qpdf_null_visibility = !options.qdf
        && options.encrypt.is_none()
        && options.copy_encryption.is_none()
        && pdf.encryption_ref().is_none()
        && pdf.deleted_object_refs().is_empty();
    let removed_refs: BTreeSet<ObjectRef> = pdf.deleted_object_refs().into_iter().collect();
    // QPDFWriter::write calls initializeSpecialStreams() -- which repairs the
    // page tree via QPDF::getAllPages() (promoting a direct /Kids leaf to a
    // fresh indirect object, cloning a duplicate leaf) -- before any object
    // numbering (QPDFWriter.cc:2113-2115, ahead of preserveObjectStreams/
    // generateObjectStreams). Run the same repair here, before the
    // Catalog-first walk below, so any object it mints is already part of
    // the graph the walk numbers. Running this after the walk (as an earlier
    // version of this function did) left freshly-minted refs outside every
    // numbering map, causing a hard failure for a page tree that needed
    // repair in QDF/content-normalization mode.
    let qdf_page_refs = if options.qdf || options.content_normalization {
        Some(crate::PageDocumentHelper::new(pdf).get_all_pages()?)
    } else {
        None
    };
    // The specialized writer is a live ObjectHandle consumer. Its
    // Catalog-first walk must therefore use the same canonical graph as the
    // emission loop; the legacy raw-Object walk would parse a content holder
    // once for numbering and make the writer report its recovery warning a
    // second time when the live handle is emitted.
    let renumber = CanonicalCatalogFirstRenumber::build_qpdf(pdf, true, false, &removed_refs)?;
    // The new /Root reference (always seeded first by the walk, so present).
    let new_root = renumber
        .new_for_original(root_ref)
        .ok_or_else(|| crate::Error::Unsupported("renumber: /Root absent from map".to_string()))?;

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
    // `encrypting` was computed once at the top (the force<1.5 gate consults it);
    // encrypt / copy_encryption are never mutated, so it is still authoritative.

    // Capture qpdf's deterministic-`/ID` seed inputs from the live source
    // trailer before the emission loop borrows `pdf`: the permanent identifier
    // `/ID[0]` (preserved when well-formed) and the `/Info`-derived seed suffix.
    // qpdf reads these from `m->pdf.getTrailer()`, not the remapped output
    // trailer, so both are gathered here while `pdf` is free.
    let (det_id_source_id0, det_id_info_suffix): (Option<Vec<u8>>, Vec<u8>) =
        if options.deterministic_id {
            let id0 = source_permanent_id(pdf.trailer());
            let suffix = deterministic_id_info_suffix(pdf);
            (id0, suffix)
        } else {
            (None, Vec::new())
        };

    // ── Step 1: run the ObjStm planner ───────────────────────────────────────
    // For --encrypt: ObjStm containers encrypt as a single blob per PDF 1.7
    // §7.5.7; the container stream is encrypted through the canonical writer
    // pipeline in the emission loop. Per-member string encryption is skipped
    // because members are not emitted in the main loop.
    // For --copy-encryption-from: keep ObjStm off (the copy path doesn't yet
    // allocate container numbers above the /Encrypt slot).
    let planner_options;
    let planner_config = if options.copy_encryption.is_some() {
        planner_options = WriterOptions {
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

    // QPDFWriter.cc:2141-2160 removes output-sensitive members only after
    // object-stream planning: encrypted output keeps the Catalog plain, while
    // linearized output also keeps page dictionaries plain. This legacy route
    // does not produce linearized output, so only output encryption applies.
    object_streams::filter_objstm_batches_for_output(pdf, &mut plan.batches, false, encrypting)?; // cov:ignore: legacy route validates /Root above and disables page traversal, so this helper cannot fail here

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

    // A forced sub-1.5 header downgrades an inherited xref-stream form to a
    // classic table: cross-reference streams are a PDF 1.5 feature, and qpdf
    // keeps the forced header and rebuilds a classic xref rather than clamping
    // the version up. Gated on the non-encrypted paths — `force<1.5 + encrypt`
    // is contradictory (the /V floor below forces >=1.5), so the encrypted
    // form/version selection is left untouched. Combined with the top-of-function
    // normalization to Disable, this makes preserve/disable/generate collapse to
    // the identical classic output under force<1.5, matching qpdf 11.9.0.
    if force_version_below_1_5(options) && !encrypting {
        effective_xref_form = XrefForm::Table;
    }

    // PDF 1.5 introduced xref streams.  Bump the header floor to 1.5 whenever
    // the chosen xref form is `Stream`, overriding even an explicit
    // `--force-version` lower than 1.5.  (A non-encrypted sub-1.5 force has
    // already been downgraded to Table just above, so this clamp now fires only
    // for the encrypted paths or a >=1.5 forced/source version.)
    if matches!(effective_xref_form, XrefForm::Stream)
        && parse_pdf_version(&version).is_none_or(|current| current < PDF_1_5)
    {
        version = "1.5".to_string();
    }

    // /V-based PDF header floor.  This fires independently of xref form: even
    // when the xref-stream bump above (lines 2102-2106) has already raised the
    // header to 1.5, a V=5/R=6 output still needs this floor to push from 1.5
    // to 1.7.  For a classic-table source with no ObjStm batches the bump does
    // not fire at all, making this floor the only mechanism that prevents e.g.
    // a 1.4 input encrypted as V=4 from emitting a spec-violating 1.4 header.
    // /V 1 (R=2) ⇒ 1.3, /V 2/R=3 ⇒ 1.4, /V 4/R=4 ⇒ 1.5 or 1.6
    // depending on the crypt filter, /V 5 ⇒ 1.7.
    if let Some(params) = options.encrypt.as_ref() {
        use crate::encrypt_setup::EncryptMethod;
        let floor = match params.method {
            EncryptMethod::V1Rc440 => PdfVersion::new(1, 3, 0),
            EncryptMethod::V2Rc4128 => PdfVersion::new(1, 4, 0),
            EncryptMethod::V4Aes128 => PdfVersion::new(1, 6, 0),
            EncryptMethod::V4Rc4128 => PDF_1_5,
            EncryptMethod::V5R6Aes256 | EncryptMethod::V5R5Aes256 => PdfVersion::new(1, 7, 0),
        };
        if parse_pdf_version(&version).is_none_or(|current| current < floor) {
            version = floor.get_version().0;
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

    // Resolve /Metadata stream ref up front for --cleartext-metadata support.
    let metadata_ref = if options
        .encrypt
        .as_ref()
        .is_some_and(|p| !p.encrypt_metadata)
        || options
            .copy_encryption
            .as_ref()
            .is_some_and(|source| !copy_encryption_encrypts_metadata(source))
    {
        resolve_metadata_stream_ref(pdf)
    } else {
        None
    };
    // ── QDF emission pre-scan ─────────────────────────────────────────────────
    // qpdf --qdf emits each stream's /Length holder IMMEDIATELY after that
    // stream object (numbered in emission order), so file positions are strictly
    // ascending 1..N with holders interleaved. Build:
    //   • qdf_emission_renumber: old_ref → emission ObjectRef (replaces CF
    //     renumber in QDF mode for renumber_refs_in_place and remap_trailer_refs)
    //   • qdf_holder_map: emission_stream_num → emission_holder_num
    // Prior-QDF-pass holder objects (bare integers reachable only via /Length
    // edges) were excluded from the CF renumber by skip_length=true; they do
    // not appear in `renumbered` and are not in qdf_emission_renumber, so the
    // main loop naturally skips them. Idempotence is achieved because every
    // pass produces the same emission ordering from the same graph structure.
    //
    // `skip_refs` is also declared here (before the pre-scan) because the
    // pre-scan applies the same skip conditions as the main loop.
    let skip_refs = removed_refs;
    let skip_ref_set: BTreeSet<ObjectRef> = skip_refs.iter().copied().collect();

    let mut qdf_emission_renumber: HashMap<ObjectRef, ObjectRef> = HashMap::new();
    let mut qdf_holder_map: HashMap<u32, u32> = HashMap::new();
    let mut qdf_max_emission: u32 = 0;

    if options.qdf {
        let mut next_emission: u32 = 0;
        for (cf_ref, old_ref) in &renumbered {
            if old_ref.number == 0 || skip_refs.contains(old_ref) {
                continue; // cov:ignore: free/deleted refs don't appear in renumbered
            }
            if member_to_batch.contains_key(old_ref) {
                continue;
            }
            // Determine whether this object is a real stream (needs a holder),
            // a non-stream object (no holder), or a structural stream that the
            // main loop skips (XRef / ObjStm).
            let object_handle = pdf.get_object_handle(*old_ref);
            pdf.resolve_object_handle(&object_handle)?;
            let is_real_stream = if object_handle.as_stream_dict().is_some() {
                let is_structural = object_handle.try_is_dictionary_of_type(b"XRef", b"")?
                    || object_handle.try_is_dictionary_of_type(b"ObjStm", b"")?;
                if is_structural {
                    None // cov:ignore: structural containers excluded from CF renumber by skip_length=true
                } else {
                    Some(true)
                }
            } else {
                Some(false)
            };
            let Some(is_stream) = is_real_stream else {
                continue; // cov:ignore: None only when is_structural; XRef/ObjStm excluded from renumbered by skip_length=true
            };

            next_emission = next_emission.checked_add(1).ok_or_else(|| {
                // cov:ignore-start: requires > 2^32 objects — impossible in practice
                crate::Error::Unsupported(
                    "full-rewrite: QDF emission number overflows u32".to_string(),
                )
            })?; // cov:ignore-end
            let emission_num = next_emission;
            qdf_emission_renumber.insert(*old_ref, ObjectRef::new(emission_num, cf_ref.generation));

            if is_stream {
                next_emission = next_emission.checked_add(1).ok_or_else(|| {
                    // cov:ignore-start: requires > 2^32 objects — impossible in practice
                    crate::Error::Unsupported(
                        "full-rewrite: QDF holder number overflows u32".to_string(),
                    )
                })?; // cov:ignore-end
                qdf_holder_map.insert(emission_num, next_emission);
            }
        }
        qdf_max_emission = next_emission;
    }

    // Generate qpdf's ordinary/static identifier once before either encryption
    // key derivation or trailer emission. The complete array is reused at every
    // trailer site so the emitted /ID[0] is the exact salt used by the context.
    let generated_id = if options.deterministic_id || options.copy_encryption.is_some() {
        None
    } else {
        Some(generate_id_array(
            pdf.trailer().get("ID"),
            options.static_id,
        ))
    };
    let generated_id_handle = generated_id.as_ref().map(generated_id_handle).transpose()?;

    // ── flpdf-9hc.4.9 / 4.11 / 4.16: encryption context ────────────────────
    // Built ONCE up front so /ID[0] is decided before any object is encrypted.
    // Compact /Encrypt follows existing objects and generated ObjStm containers.
    // QDF /Encrypt follows the final interleaved /Length holder from the pre-scan.
    let encrypt_ctx: Option<EncryptionContext> = if let Some(ref params) = options.encrypt {
        let base_for_encrypt = if options.qdf {
            qdf_max_emission
        } else {
            // cov:ignore-start: contiguous object and batch counts cannot approach u32::MAX in a supported process.
            let containers_len = u32::try_from(plan.batches.len()).map_err(|_| {
                crate::Error::Unsupported(
                    "full-rewrite encrypt: ObjStm batch count overflows u32".to_string(),
                )
            })?;
            existing_max.checked_add(containers_len).ok_or_else(|| {
                crate::Error::Unsupported(
                    "full-rewrite encrypt: /Encrypt object number overflows u32".to_string(),
                )
            })?
            // cov:ignore-end
        };
        let id0 = generated_id
            .as_ref()
            .and_then(Object::as_array)
            .and_then(|values| values.first())
            .and_then(Object::as_string)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| {
                // cov:ignore-start: generate_id_array always returns a valid two-string array here
                crate::Error::Unsupported(
                    "full-rewrite: ordinary/static ID generator returned an invalid /ID array"
                        .to_string(),
                )
                // cov:ignore-end
            })?; // cov:ignore: invalid ID guard is unreachable after generate_id_array
        let context =
            build_encryption_context(options, params, base_for_encrypt, metadata_ref, &id0);
        Some(context?)
    } else if let Some(ref src) = options.copy_encryption {
        let base_for_encrypt = if options.qdf {
            qdf_max_emission
        } else {
            existing_max
        };
        Some(build_copy_encryption_context(
            src,
            options,
            base_for_encrypt,
            metadata_ref,
        )?)
    } else {
        None
    };
    let mut encrypted_strings = encrypt_ctx
        .as_ref()
        .map(encrypted_strings::EncryptedStringEmitter::from_context);

    // ── QDF page/contents marker pre-scan ─────────────────────────────────────
    // qpdf --qdf emits two page-context comments to help human readers:
    //   • "%% Page N\n"              — immediately before each Page dict's
    //                                   "M G obj" line (N is 1-based page order)
    //   • "%% Contents for page N\n" — immediately before each content stream's
    //                                   "M G obj" line (N is the owning page's
    //                                   1-based order); a page's /Contents may
    //                                   be a lone reference or an array of
    //                                   references, and every element shares the
    //                                   same page number.
    // The contents map also selects exactly the indirect page-content streams
    // eligible for qpdf content normalization. Maps are keyed on ORIGINAL
    // ObjectRefs (matching how the emit loop compares via `old_ref`). Page
    // markers are populated and emitted only in QDF mode. They ride ahead of
    // "%% Original object ID:" and are NOT suppressed by
    // no_original_object_ids. Mirrors qpdf 11.9.0 QPDFWriter.cc:1774-1785.
    //
    // `contents_seq` contains only terminal indirect stream refs returned by
    // the shared page-content resolver. `content_container_refs` identifies
    // page dictionaries and indirect array holders that contain direct Stream
    // values; those values have no ObjectRef of their own and must be
    // normalized in the containing object during emission.
    let (page_seq, contents_seq, content_container_refs): (
        HashMap<ObjectRef, u32>,
        HashMap<ObjectRef, u32>,
        BTreeSet<ObjectRef>,
    ) = if options.qdf || options.content_normalization {
        let mut page_seq: HashMap<ObjectRef, u32> = HashMap::new();
        let mut contents_seq: HashMap<ObjectRef, u32> = HashMap::new();
        let mut content_container_refs = BTreeSet::new();
        // QPDFWriter::initializeSpecialStreams delegates page enumeration to
        // QPDF::getAllPages(), whose live ObjectHandle lookup accepts a direct
        // Catalog /Pages dictionary (QPDFWriter.cc:1916; QPDF_pages.cc:47-71).
        // The repair-and-enumerate pass already ran once, before the
        // Catalog-first numbering walk above, so any object it mints is
        // numbered; reuse that snapshot instead of repairing (a no-op the
        // second time) and enumerating again.
        let page_refs = qdf_page_refs
            .as_ref()
            .expect("qdf_page_refs is Some whenever options.qdf || options.content_normalization");
        for (idx, page_ref) in page_refs.iter().enumerate() {
            let seq = (idx as u32).saturating_add(1);
            if options.qdf {
                page_seq.insert(*page_ref, seq);
            }
            // Follow the page's `/Contents` holders through the canonical
            // ObjectHandle graph. The legacy page helper materializes the
            // same chain and would make a damaged stream's parser warning
            // appear twice when the emission loop later resolves its live
            // handle; qpdf's writer stays on one object cache throughout.
            for terminal_ref in collect_content_stream_refs_tolerant(pdf, *page_ref)? {
                contents_seq.insert(terminal_ref, seq);
            }
            collect_content_container_refs(pdf, *page_ref, &mut content_container_refs)?;
        }
        (page_seq, contents_seq, content_container_refs)
    } else {
        (HashMap::new(), HashMap::new(), BTreeSet::new())
    };

    // In QDF mode, /Root's ref in the trailer is in emission-space; rebind
    // new_root from the qdf_emission_renumber map so remap_trailer_refs and the
    // explicit trailer.insert("Root", ...) both use the same emission number.
    let new_root = if options.qdf {
        qdf_emission_renumber
            .get(&root_ref)
            .copied()
            .ok_or_else(|| {
                // cov:ignore-start: /Root is always reachable from the BFS seed, so it
                // is always in renumbered and therefore always in qdf_emission_renumber.
                crate::Error::Unsupported(
                    "QDF emission: /Root absent from emission map".to_string(),
                )
            })? // cov:ignore-end
    } else {
        new_root
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    if options.pclm {
        bytes.extend_from_slice(b"%PCLm 1.0\n"); // cov:ignore: PCLm returns through write_pclm before this coordinator
    } else {
        bytes.extend_from_slice(QPDF_BINARY_MARKER);
    }
    if options.qdf {
        bytes.extend_from_slice(b"%QDF-1.0\n");
        bytes.extend_from_slice(b"\n");
    }
    bytes.extend_from_slice(options.extra_header_text.as_bytes());

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();
    let mut emitted_old_to_new = BTreeMap::<ObjectRef, ObjectRef>::new();

    for (new_ref, old_ref) in &renumbered {
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

        // In QDF mode, look up the emission-space ObjectRef. Objects absent
        // from qdf_emission_renumber (prior-QDF-pass holders excluded by
        // skip_length=true in CF renumber) are skipped here, ensuring
        // idempotence. In non-QDF mode emit_ref == *new_ref.
        let emit_ref = if options.qdf {
            match qdf_emission_renumber.get(old_ref) {
                Some(&r) => r,
                None => continue, // cov:ignore: pre-scan and main loop have symmetric skips; unreachable in valid PDFs
            }
        } else {
            *new_ref
        };

        // Resolve the live ObjectHandle once. qpdf's writer keeps this handle
        // as the source of truth and remaps only reference tokens while
        // unparsing; it does not materialize the whole object graph before
        // emission.
        let object_handle = pdf.get_object_handle(*old_ref);
        pdf.resolve_object_handle(&object_handle)?;
        let is_stream = object_handle.as_stream_dict().is_some();

        // Direct `/Contents` streams have no terminal ObjectRef to put in
        // `contents_seq`. Their owning page/array holder uses the dedicated
        // handle-native content-container serializer below; this applies in
        // both QDF and normalization modes because a generic child serializer
        // intentionally emits only a direct stream's dictionary.
        let content_container = content_container_refs.contains(old_ref);
        // Skip xref-stream and ObjStm container objects — we'll rebuild the
        // structural streams from scratch below. Handle predicates preserve
        // qpdf's live dictionary lookup without resolving a legacy `Object`.
        if is_stream
            && (object_handle.try_is_dictionary_of_type(b"XRef", b"")?
                || object_handle.try_is_dictionary_of_type(b"ObjStm", b"")?)
        {
            continue; // cov:ignore: structural streams are rebuilt by their dedicated loops below
        }

        // Duplicate detection: `offsets` is keyed on the emitted number.
        if offsets.contains_key(&emit_ref.number) {
            // cov:ignore-start: qdf_emission_renumber assigns unique sequential numbers,
            // so collisions cannot occur in valid PDFs; this is a bug-detection guard.
            return Err(crate::Error::Unsupported(format!(
                "duplicate object number {} in xref table",
                emit_ref.number
            )));
            // cov:ignore-end
        }

        // QDF page/contents markers ride ahead of "%% Original object ID:" and
        // remain even under no_original_object_ids. Mirrors qpdf 11.9.0
        // QPDFWriter.cc:1774-1785. Keyed on original refs (old_ref).
        if options.qdf {
            if let Some(&seq) = page_seq.get(old_ref) {
                bytes.extend_from_slice(format!("%% Page {seq}\n").as_bytes());
            }
            if let Some(&seq) = contents_seq.get(old_ref) {
                bytes.extend_from_slice(format!("%% Contents for page {seq}\n").as_bytes());
            }
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

        // The body header uses the emitted number.
        let emit_offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", emit_ref.number, emit_ref.generation).as_bytes(),
        );

        // Will be set to Some((holder_num, len_value, ignore_newline)) for QDF
        // streams so we can emit the marker and holder immediately after the
        // stream's endobj.
        let mut qdf_holder_to_emit: Option<(u32, i64, bool)> = None;

        let map = |object_ref: ObjectRef| {
            if options.qdf {
                qdf_emission_renumber
                    .get(&object_ref)
                    .copied()
                    .ok_or_else(|| {
                        // cov:ignore-start: catalog-first planning inserts every live QDF reference
                        crate::Error::Unsupported(format!(
                            "full-rewrite: QDF reference {object_ref} absent from emission map"
                        ))
                        // cov:ignore-end
                    }) // cov:ignore: catalog-first planning makes every QDF reference resolvable
            } else {
                renumber.new_for_original(object_ref).ok_or_else(|| {
                    // cov:ignore-start: catalog-first planning inserts every live reference
                    crate::Error::Unsupported(format!(
                        "full-rewrite: reference {object_ref} absent from renumber map"
                    ))
                    // cov:ignore-end
                }) // cov:ignore: catalog-first planning makes every reference resolvable
            }
        };
        let removed_refs: BTreeSet<ObjectRef> = skip_refs.iter().copied().collect();

        if content_container {
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_content_container_with_ref_map(
                    &mut bytes,
                    emit_ref,
                    None,
                    &object_handle,
                    options,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: LLVM does not attribute the successful encrypted emitter continuation
            } else {
                plain::body::emit_content_container_from_handle_with_ref_map(
                    &object_handle,
                    options,
                    &mut bytes,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: LLVM does not attribute the successful plain emitter continuation
            }
        } else if is_stream {
            // This is the qpdf stream writer's live-handle path: filtering and
            // payload framing are decided from the stream handle, while the
            // dictionary serializer remaps only child reference tokens.
            let (stream_dict, stream_data, refiltered) =
                plain::body::canonical_stream_output_for_rewrite(
                    &object_handle,
                    options,
                    options.content_normalization && contents_seq.contains_key(old_ref),
                )?; // cov:ignore: canonical stream output is validated before this success continuation
            let stream_encryption = encrypt_ctx
                .as_ref()
                .filter(|ctx| emit_ref != ctx.encrypt_ref);
            let encrypt_stream = stream_encryption
                .is_some_and(|ctx| ctx.encrypt_metadata || ctx.metadata_ref != Some(*old_ref));
            let stream_dict = stream_dict;
            let mut stream_length = stream_data.len();
            if let Some(ctx) = stream_encryption {
                adjust_aes_stream_length(&mut stream_length, ctx, encrypt_stream)?;
            }
            stream_dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                    // cov:ignore-start: an allocatable stream payload fits in i64
                    crate::Error::Unsupported("stream /Length does not fit in i64".to_string())
                    // cov:ignore-end
                })?), // cov:ignore: an allocatable stream payload fits in i64
            )?; // cov:ignore: validated stream /Length replacement; LLVM maps this continuation to the call setup

            let holder_ref = if options.qdf {
                let holder_num =
                    qdf_holder_map
                        .get(&emit_ref.number)
                        .copied()
                        .ok_or_else(|| {
                            // cov:ignore-start: the QDF pre-scan creates a holder for every emitted stream
                            crate::Error::Unsupported(format!(
                                "full-rewrite: QDF holder not found for stream at emission {}",
                                emit_ref.number
                            ))
                            // cov:ignore-end
                        })?; // cov:ignore: the QDF pre-scan creates a holder for every emitted stream
                Some(ObjectRef::new(holder_num, 0))
            } else {
                None
            };
            let stream_options =
                encrypted_strings::StreamDictOptions::new(options.qdf, refiltered, encrypt_stream);
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_stream_dict_with_ref_map(
                    &mut bytes,
                    emit_ref,
                    None,
                    &stream_dict,
                    stream_options,
                    &map,
                    &removed_refs,
                    holder_ref,
                )?; // cov:ignore: handle-native stream dictionary route; LLVM maps the call continuation here
            } else if options.qdf {
                stream_dict.unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
                    &mut bytes,
                    0,
                    &map,
                    &removed_refs,
                    holder_ref,
                )?; // cov:ignore: handle-native QDF stream dictionary route; LLVM maps the call continuation here
            } else {
                stream_dict.unparse_stream_body_with_ref_map_and_removed(
                    &mut bytes,
                    refiltered,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: handle-native stream dictionary route; LLVM maps the call continuation here
            }

            let added_newline = if let Some(ctx) = stream_encryption {
                write_stream_payload_with_pipeline(
                    &mut bytes,
                    &stream_data,
                    options.newline_before_endstream,
                    emit_ref,
                    ctx,
                    encrypt_stream,
                    None,
                )? // cov:ignore: encrypted stream payload route; LLVM maps the call continuation here
            } else {
                serialize::write_stream_payload(
                    &mut bytes,
                    &stream_data,
                    options.newline_before_endstream,
                );
                serialize::framing_adds_newline(&stream_data, options.newline_before_endstream)
            };
            if let Some(holder_ref) = holder_ref {
                qdf_holder_to_emit = Some((
                    holder_ref.number,
                    i64::try_from(stream_length).unwrap_or(i64::MAX),
                    added_newline,
                ));
            }
        } else {
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_object_with_ref_map(
                    &mut bytes,
                    emit_ref,
                    None,
                    &object_handle,
                    options.qdf,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: encrypted handle-object route; LLVM maps the call continuation here
            } else if options.qdf {
                object_handle.unparse_object_qdf_with_ref_map_and_removed(
                    &mut bytes,
                    0,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: QDF handle-object route; LLVM maps the call continuation here
            } else {
                object_handle.unparse_object_with_ref_map_and_removed(
                    &mut bytes,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: compact handle-object route; LLVM maps the call continuation here
            }
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
        offsets.insert(emit_ref.number, (emit_ref.generation, emit_offset));
        emitted_old_to_new.insert(*old_ref, ObjectRef::new(emit_ref.number, 0));
        report_progress_event(options);

        // QDF: emit the length-holder object IMMEDIATELY after its stream's
        // endobj + blank line, numbered in sequential emission order so that
        // object file positions are strictly ascending 1..N (qpdf 11.9.0
        // behaviour). No "%% Original object ID:" comment for holder objects
        // (they are synthetic; qpdf only emits that comment for source objects).
        if let Some((hnum, hlen, ignore_newline)) = qdf_holder_to_emit {
            if ignore_newline {
                bytes.extend_from_slice(b"%QDF: ignore_newline\n");
            }
            let h_offset = bytes.len();
            bytes.extend_from_slice(format!("{hnum} 0 obj\n{hlen}\nendobj\n").as_bytes());
            bytes.push(b'\n'); // QDF inter-object blank line
            offsets.insert(hnum, (0, h_offset));
        }
    }

    // ── Step 5: emit each ObjStm container ───────────────────────────────────
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_ref = container_refs[batch_idx];
        // Resolve each member as a live ObjectHandle and remap only its child
        // reference tokens during emission. The encrypted branch remains on
        // the legacy callback until the handle-aware string-writer adapter is
        // wired into the same boundary.
        let mut handles = Vec::with_capacity(batch.len());
        for &old in batch {
            let handle = pdf.get_object_handle(old);
            pdf.resolve_object_handle(&handle)?;
            let new = renumber.new_for_original(old).ok_or_else(|| {
                crate::Error::Unsupported("ObjStm member absent from renumber map".to_string())
            })?;
            emitted_old_to_new.insert(old, ObjectRef::new(new.number, 0));
            handles.push((new, handle));
        }
        let removed_refs: BTreeSet<ObjectRef> = skip_refs.iter().copied().collect();
        let map = |object_ref: ObjectRef| {
            renumber.new_for_original(object_ref).ok_or_else(|| {
                // cov:ignore-start: ObjStm members are selected from the same complete renumber map
                crate::Error::Unsupported(format!(
                    "full-rewrite: ObjStm reference {object_ref} absent from renumber map"
                ))
                // cov:ignore-end
            }) // cov:ignore: ObjStm members are selected from the same complete renumber map
        };
        let body = object_streams::emit_objstm_body_from_handles_with_writer(
            &handles,
            &mut |out, _member_index, _member_ref, handle| {
                let result =
                    handle.unparse_object_with_ref_map_and_removed(out, &map, &removed_refs);
                if result.is_ok() {
                    report_progress_event(options);
                }
                result
            },
        )?; // cov:ignore: handle-native ObjStm member emission; LLVM maps the call continuation here
        let (stream_handle, stream_data) =
            object_streams::wrap_objstm_body_as_handle(&body, options.compress_streams, None)?;
        let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
            // cov:ignore-start: wrap_objstm_body_as_handle constructs a stream unconditionally
            crate::Error::Internal("ObjStm handle lost its stream dictionary".to_string())
            // cov:ignore-end
        })?; // cov:ignore: wrap_objstm_body_as_handle constructs a stream unconditionally
        let mut stream_length = stream_data.len();
        if let Some(ctx) = &encrypt_ctx {
            adjust_aes_stream_length(&mut stream_length, ctx, true)?;
        }
        stream_dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                // cov:ignore-start: an allocatable ObjStm payload fits in i64
                crate::Error::Unsupported(
                    "encrypted ObjStm /Length does not fit in i64".to_string(),
                )
                // cov:ignore-end
            })?), // cov:ignore: an allocatable ObjStm payload fits in i64
        )?; // cov:ignore: validated ObjStm /Length replacement; LLVM maps the call continuation here

        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", container_ref.number).as_bytes());
        // Encrypt the ObjStm container as a single blob (PDF 1.7 §7.5.7).
        // Member objects' strings are NOT individually encrypted; the container
        // stream's encryption covers them all.
        let identity_map = |object_ref: ObjectRef| Ok(object_ref);
        let no_removed_refs = BTreeSet::new();
        if let Some(ctx) = &encrypt_ctx {
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_stream_dict_with_ref_map(
                    &mut bytes,
                    container_ref,
                    None,
                    &stream_dict,
                    encrypted_strings::StreamDictOptions::new(false, false, true),
                    &identity_map,
                    &no_removed_refs,
                    None,
                )?; // cov:ignore: encrypted ObjStm dictionary route; LLVM maps the call continuation here
            } else {
                // cov:ignore-start: encrypted output always constructs the handle-aware emitter
                stream_dict.unparse_stream_body_with_ref_map_and_removed(
                    &mut bytes,
                    false,
                    &identity_map,
                    &no_removed_refs,
                )?;
                // cov:ignore-end
            }
            write_stream_payload_with_pipeline(
                &mut bytes,
                &stream_data,
                options.newline_before_endstream,
                container_ref,
                ctx,
                true,
                None,
            )?; // cov:ignore: the encrypted ObjStm route executes; this call continuation has no counter.
        } else {
            stream_dict.unparse_stream_body_with_ref_map_and_removed(
                &mut bytes,
                false,
                &identity_map,
                &no_removed_refs,
            )?; // cov:ignore: plain ObjStm payload route; LLVM maps the call continuation here
            serialize::write_stream_payload(
                &mut bytes,
                &stream_data,
                options.newline_before_endstream,
            );
        }
        bytes.extend_from_slice(b"\nendobj\n");
        // QDF inter-object blank-line separator (flpdf-9hc.6.10). qdf mode
        // emits no ObjStm containers (6.2), so this is a consistency guard.
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(container_ref.number, (0, emit_offset));
    }

    // ── flpdf-9hc.4.9: emit the /Encrypt dictionary as a plaintext indirect
    // object. Per PDF 1.7 §7.6.1 the /Encrypt dict itself is never encrypted;
    // its strings (/U /O /UE /OE /Perms) are already in their final wire form
    // from the dict builders.
    if let Some(ctx) = &encrypt_ctx {
        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", ctx.encrypt_ref.number).as_bytes());
        let encrypt_handle = ctx.encrypt_dict_handle();
        encrypted_strings::write_encryption_dictionary_handle(&mut bytes, &encrypt_handle)?;
        bytes.extend_from_slice(b"\nendobj\n");
        if options.qdf {
            bytes.push(b'\n');
        }
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

    let mut written_xref = BTreeMap::<ObjectRef, XrefEntry>::new();
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
            for number in 1..object_count {
                let object_number = number as u32;
                if let Some(&(_generation, offset)) = offsets.get(&object_number) {
                    written_xref.insert(
                        ObjectRef::new(object_number, 0),
                        XrefEntry::Uncompressed {
                            // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                            // on every supported target.
                            offset: u64::try_from(offset).map_err(|_| {
                                crate::Error::Unsupported(
                                    "xref offset does not fit u64".to_string(),
                                )
                            })?,
                            // cov:ignore-end
                        },
                    );
                } // cov:ignore: LLVM maps the covered contiguous-xref branch exit to this brace
            }

            // Trailer — start from the document trailer, strip incremental keys.
            let trailer = build_writer_trailer_handle(
                pdf,
                pdf.last_xref_form() == XrefForm::Stream,
                false,
                object_count,
                new_root,
                options,
                encrypt_ctx.as_ref(),
                options.deterministic_id,
                generated_id_handle.as_ref(),
            )?; // cov:ignore: validated writer trailer construction; LLVM maps this continuation to the call setup
            let trailer_map = |object_ref: ObjectRef| {
                if options.qdf {
                    qdf_emission_renumber
                        .get(&object_ref)
                        .copied()
                        .ok_or_else(|| {
                            // cov:ignore-start: catalog-first planning inserts every live QDF trailer reference
                            crate::Error::Unsupported(format!(
                                "full-rewrite: QDF trailer reference {object_ref} absent from emission map"
                            ))
                            // cov:ignore-end
                        }) // cov:ignore: catalog-first planning makes every QDF trailer reference resolvable
                } else {
                    renumber.new_for_original(object_ref).ok_or_else(|| {
                        // cov:ignore-start: catalog-first planning inserts every live trailer reference
                        crate::Error::Unsupported(format!(
                            "full-rewrite: trailer reference {object_ref} absent from renumber map"
                        ))
                        // cov:ignore-end
                    }) // cov:ignore: catalog-first planning makes every trailer reference resolvable
                }
            };

            if options.qdf {
                // qpdf --qdf trailer: "trailer <<" on one line, then one
                // "  /Key value" entry per line with the keys alphabetically
                // sorted but /ID and /Encrypt forced last in that order
                // (verified against qpdf 11.9.0: minimal => /Root /Size /ID;
                // encrypted => /Info /Root /Size /ID /Encrypt, with the final
                // two entries on one line). Values use the EXISTING compact
                // serializer, which keeps the /ID array inline
                // ("[<hex><hex>]") — do NOT route the trailer through the qdf
                // dict serializer. Closing ">>" then startxref directly (no
                // extra leading newline) to match the qpdf reference.
                if options.deterministic_id {
                    let mut id_writer = |out: &mut Vec<u8>| {
                        write_deterministic_id_inline(
                            out,
                            &det_id_info_suffix,
                            det_id_source_id0.as_deref(),
                        )
                    };
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.unparse_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        true,
                        Some(&mut id_writer),
                        &trailer_map,
                        &skip_ref_set,
                        qpdf_null_visibility,
                    )?;
                    // cov:ignore-end
                } else {
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.unparse_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        true,
                        None,
                        &trailer_map,
                        &skip_ref_set,
                        qpdf_null_visibility,
                    )?;
                    // cov:ignore-end
                }
                bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
            } else {
                // qpdf classic trailer: the dict sits on the `trailer ` line
                // (single space, not its own line) with keys sorted but /ID
                // forced last — `trailer << /Info .. /Root .. /Size N /ID [..]
                // >>` (verified against qpdf 11.9.0 static-id goldens).
                if options.deterministic_id {
                    let mut id_writer = |out: &mut Vec<u8>| {
                        write_deterministic_id_inline(
                            out,
                            &det_id_info_suffix,
                            det_id_source_id0.as_deref(),
                        )
                    };
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.unparse_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        false,
                        Some(&mut id_writer),
                        &trailer_map,
                        &skip_ref_set,
                        qpdf_null_visibility,
                    )?;
                    // cov:ignore-end
                } else {
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.unparse_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        false,
                        None,
                        &trailer_map,
                        &skip_ref_set,
                        qpdf_null_visibility,
                    )?;
                    // cov:ignore-end
                }
                bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
            }
        }

        XrefForm::Stream => {
            // Cross-reference stream emission is delegated to the canonical
            // plain-writer xref layer below.
            // cov:ignore-start: object_count is bounded by the u32 object-number
            // space before this branch, so this usize overflow requires an
            // unallocatable PDF-sized object universe.
            let xref_size = object_count.checked_add(1).ok_or_else(|| {
                crate::Error::Unsupported("full-rewrite: xref-stream /Size overflows usize".into())
            })?;
            // cov:ignore-end
            let old_to_new: HashMap<ObjectRef, ObjectRef> = renumbered
                .iter()
                .map(|(new_ref, old_ref)| (*old_ref, *new_ref))
                .collect();
            let layout = plain::xref::BodyLayout {
                uncompressed: offsets.clone(),
                compressed: member_new_to_batch
                    .iter()
                    .map(|(&number, &(container, index))| {
                        (number, plain::xref::CompressedLocation { container, index })
                    })
                    .collect(),
            };
            let trailer_handle = build_writer_trailer_handle(
                pdf,
                pdf.last_xref_form() == XrefForm::Stream,
                true,
                xref_size,
                new_root,
                options,
                encrypt_ctx.as_ref(),
                options.deterministic_id,
                generated_id_handle.as_ref(),
            )?; // cov:ignore: validated xref trailer construction; LLVM maps this continuation to the call setup
            let id = plain::xref::IdPlan::Materialized {
                value: plain::xref::materialized_id_handle(&trailer_handle.try_get_key(b"/ID")?)?, // cov:ignore: build_writer_trailer_handle constructs the writer-owned /ID in the validated two-string shape
            };
            let trailer = plain::xref::TrailerPlan {
                form: XrefForm::Stream,
                canonical_entries: plain::plan::canonical_trailer_entries_with_visibility(
                    pdf,
                    &old_to_new,
                    &skip_ref_set,
                    qpdf_null_visibility,
                )?, // cov:ignore: live trailer references are validated by the canonical map
                root: new_root,
                id,
                encrypt: encrypt_ctx.as_ref().map(|ctx| ctx.encrypt_ref),
                structural_filtered: matches!(options.compress_streams, CompressStreams::Yes),
            };
            written_xref = plain::xref::append_xref_and_trailer(&mut bytes, &layout, &trailer)?;
        }
    }

    out.write_all(&bytes)?;
    Ok(WriterResult::new(emitted_old_to_new, written_xref))
}

/// Apply the stream compression policy to a single stream object.
///
/// This is the choke-point for re-emitting **regular indirect stream
/// objects** in the canonical rewrite path. The cross-reference stream and
/// object-stream (ObjStm) containers apply the same `CompressStreams`
/// policy on their own dedicated branches (the xref-stream branch below
/// and `object_streams::wrap_objstm_body`); they do not flow through
/// this function. QDF mode is exempt because it has its own stream framing and
/// decode policy.
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
/// flpdf) or because the stream data is corrupt — the stream's `/Filter`
/// chain and data bytes are returned **verbatim**.  This preserves
/// readability: a PDF reader that understands the codec can still decode the
/// stream, and we do not corrupt the data by emitting uninterpreted bytes
/// under a wrong (or missing) filter declaration.  The one normalization
/// applied even on this path is `/Length`: qpdf writes every emitted stream's
/// `/Length` as a direct integer (the raw byte count), never an indirect
/// reference, so a source carrying `/Length M G R` has it directized to
/// `data.len()` here (the data bytes are untouched, so the value is unchanged
/// for a well-formed direct length).
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
    // This public helper predates PdfWriter's decode-level setting. Preserve
    // its contract of decoding every filter implemented by flpdf; only the
    // private PdfWriter bridge applies the configured qpdf decode-level gate.
    apply_stream_compress_policy_with_decode_level(stream, policy, DecodeLevel::All, false)
}

fn apply_stream_compress_policy_with_decode_level(
    stream: &crate::Stream,
    policy: CompressStreams,
    decode_level: DecodeLevel,
    normalize_content: bool,
) -> Object {
    if !filter_chain_is_decodable(
        stream.dict.get("Filter"),
        policy,
        decode_level,
        normalize_content,
    ) {
        let mut dict = stream.dict.clone();
        dict.insert(
            "Length",
            Object::Integer(i64::try_from(stream.data.len()).unwrap_or(i64::MAX)),
        );
        return Object::Stream(crate::Stream::new(dict, stream.data.clone()));
    }

    // A filter above the selected level has already returned through the raw
    // chain-preservation branch above, matching qpdf's all-or-nothing gate.
    // For an in-level chain, `decode_stream_data` is the single owner of
    // `/DecodeParms` shape alignment and per-filter parameter validation (the
    // responsibility corresponding to QPDF_Stream::filterable). Keep that
    // validation in the existing decoder instead of duplicating its parser
    // here; any resulting Err takes the raw-preservation fallback below.
    let decoded = match filters::decode_stream_data(&stream.dict, &stream.data) {
        Ok(d) => d,
        Err(_) => {
            // Decode failure (unsupported codec or corrupt data): emit the data
            // and /Filter chain verbatim so downstream readers (e.g. image
            // renderers) can still interpret the stream correctly. qpdf, however,
            // writes EVERY emitted stream's /Length as a direct integer, never an
            // indirect reference; directize it here so a source carrying
            // `/Length M G R` does not leak an indirect /Length (and a renumbered
            // holder reference) into the output — a byte divergence from qpdf for
            // passthrough/non-decodable streams whose length holder is kept live
            // by another reference (flpdf-q1j2). The data bytes are untouched, so
            // /Length equals stream.data.len().
            let mut dict = stream.dict.clone();
            dict.insert(
                "Length",
                Object::Integer(i64::try_from(stream.data.len()).unwrap_or(i64::MAX)),
            );
            return Object::Stream(crate::Stream::new(dict, stream.data.clone()));
        }
    };
    let decoded = if normalize_content {
        crate::normalize_content_stream(&decoded).into_bytes()
    } else {
        decoded
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
                Err(_) => return Object::Stream(stream.clone()), // cov:ignore: in-memory Flate encoding failures are not injectable through supported writer input
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

/// Apply qpdf's decode-level gate to the entire filter chain. qpdf does not
/// partially decode a chain: one filter above the selected level, or one
/// filter it cannot filter, makes the complete chain non-filterable.
///
/// `QPDF_Stream.cc:504-512,537-542` makes one important distinction: a
/// compress or content-normalization request supplies an encode flag, so
/// generalized filters are filterable even at decode level `none`; a plain
/// uncompress request at `none` preserves them. Specialized filters remain
/// gated by the selected level in every policy.
fn filter_chain_is_decodable(
    filter: Option<&Object>,
    policy: CompressStreams,
    decode_level: DecodeLevel,
    normalize_content: bool,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let filters = match filter {
        Object::Null => return true,
        Object::Name(_) => std::slice::from_ref(filter),
        Object::Array(filters) => filters.as_slice(),
        _ => return false,
    };

    filters.iter().all(|filter| {
        let Some(name) = filter.as_name() else {
            return false;
        };
        let name = match name {
            b"Fl" => b"FlateDecode".as_slice(),
            b"LZW" => b"LZWDecode".as_slice(),
            b"A85" => b"ASCII85Decode".as_slice(),
            b"AHx" => b"ASCIIHexDecode".as_slice(),
            b"RL" => b"RunLengthDecode".as_slice(),
            name => name,
        };
        match name {
            b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode" => {
                !matches!(decode_level, DecodeLevel::None)
                    || policy == CompressStreams::Yes
                    || normalize_content
            }
            b"RunLengthDecode" => {
                matches!(decode_level, DecodeLevel::Specialized | DecodeLevel::All)
            }
            _ => false,
        }
    })
}

/// Whether a stream's `/Filter` value is a lone `/FlateDecode` bare name.
/// qpdf's `QPDFWriter.cc:1265-1269` fast path recognizes the name form
/// (including qpdf's `/Fl` abbreviation), but not a single-element array.
pub(crate) fn is_lone_flate(filter: Option<&Object>) -> bool {
    match filter {
        Some(Object::Name(name)) => name.as_slice() == b"FlateDecode" || name.as_slice() == b"Fl",
        _ => false,
    }
}

/// Collect indirect objects that can contain direct streams inside a page's
/// `/Contents` value. Terminal indirect streams are tracked separately by
/// [`crate::pages::page_content_stream_entries_tolerant`]; this helper only
/// records page dictionaries and indirect array holders so the emission loop
/// can replace their direct Stream values in place.
fn collect_content_container_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    containers: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    enum ContentsValue {
        DirectStream,
        DirectArray(Vec<ObjectRef>),
        Indirect(ObjectRef),
    }
    let page_handle = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page_handle)?;
    let contents_handle = page_handle.try_get_key(b"/Contents")?;
    let contents = if let Some(reference) = contents_handle
        .object_ref()
        .or_else(|| contents_handle.as_reference())
    {
        // A resolved indirect stream/array remains an indirect object for
        // writer ownership purposes. Only a direct stream/array nested in the
        // page dictionary belongs to the containing page object.
        Some(ContentsValue::Indirect(reference))
    } else if contents_handle.as_stream_dict().is_some() {
        Some(ContentsValue::DirectStream)
    } else {
        contents_handle.try_as_array()?.map(|items| {
            ContentsValue::DirectArray(
                items
                    .iter()
                    .filter_map(|item| item.object_ref().or_else(|| item.as_reference()))
                    .collect(),
            )
        })
    };
    match contents {
        Some(ContentsValue::DirectStream) => {
            containers.insert(page_ref);
        }
        Some(ContentsValue::DirectArray(refs)) => {
            containers.insert(page_ref);
            for reference in refs {
                collect_content_array_holder_refs(pdf, reference, containers)?;
            }
        }
        Some(ContentsValue::Indirect(reference)) => {
            collect_content_array_holder_refs(pdf, reference, containers)?;
        }
        _ => {}
    }
    Ok(())
}

/// Collect terminal indirect stream references from a page's `/Contents`
/// value without materializing the page-content holder chain. Direct streams
/// and non-stream values are intentionally omitted: the former have no object
/// identity for `contents_seq`, and the latter are the tolerant writer shape
/// qpdf skips while still rewriting valid sibling streams.
fn collect_content_stream_refs_tolerant<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    let page_handle = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page_handle)?;
    let contents_handle = page_handle.try_get_key(b"/Contents")?;
    let (contents, contents_ref) = pdf.resolve_object_handle_to_terminal_ref(&contents_handle)?;

    if contents.try_is_null()? {
        return Ok(Vec::new());
    }
    if contents.as_stream_dict().is_some() {
        return Ok(contents_ref.into_iter().collect());
    }

    let Some(items) = contents.try_as_array()? else {
        return Ok(Vec::new());
    };
    let mut refs = Vec::with_capacity(items.len());
    for item in items {
        let (item, item_ref) = pdf.resolve_object_handle_to_terminal_ref(&item)?;
        if item.as_stream_dict().is_some() {
            if let Some(item_ref) = item_ref {
                refs.push(item_ref);
            }
        }
    }
    Ok(refs)
}

/// Follow one `/Contents` holder chain until it reaches an array or stream.
/// When it reaches an array, retain the array holder and inspect its reference
/// elements for nested array holders. Direct stream elements are handled when
/// their containing array object is emitted.
fn collect_content_array_holder_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: ObjectRef,
    containers: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let mut pending = VecDeque::from([(start, 0_usize)]);
    let mut visited = BTreeSet::new();

    while let Some((current, depth)) = pending.pop_front() {
        if depth >= crate::ref_chain::MAX_REF_CHAIN_DEPTH || !visited.insert(current) {
            continue;
        }
        let handle = pdf.get_object_handle(current);
        pdf.resolve_object_handle(&handle)?;
        if let Some(next) = handle.as_reference() {
            pending.push_back((next, depth + 1));
        } else if let Some(items) = handle.try_as_array()? {
            containers.insert(current);
            pending.extend(
                items
                    .iter()
                    .filter_map(|item| item.object_ref().or_else(|| item.as_reference()))
                    .map(|reference| (reference, depth + 1)),
            );
        }
    }
    Ok(())
}

/// QDF variant of [`write_stream_to_buf`]: identical stream/endstream framing
/// and newline-before-endstream behaviour, but the stream dictionary is
/// serialized with the qpdf `--qdf` multi-line / sorted-key layout
/// ([`Dictionary::write_pdf_qdf`]) instead of the compact form. Used only on
/// the qdf full-rewrite path; preserves the 6.1-era stream invariants
/// (raw `data`; encrypted `/Length` is derived immediately before unparse).
#[cfg(test)]
fn write_stream_to_buf_qdf(
    buf: &mut Vec<u8>,
    stream: &crate::Stream,
    policy: NewlineBeforeEndstream,
    encrypted_strings: Option<&mut encrypted_strings::EncryptedStringEmitter>,
    emitted_ref: ObjectRef,
    stream_encryption: Option<&EncryptionContext>,
    encrypt_stream_strings: bool,
) -> Result<bool> {
    // qpdf --qdf pulls /Length past every other (alphabetically-sorted) key so
    // the length indirect reference sits immediately before `>>`; use the
    // /Length-last QDF serializer here (mirrors non-QDF write_pdf_stream).
    if let Some(ctx) = stream_encryption {
        let mut dict = stream.dict.clone();
        if !matches!(dict.get("Length"), Some(Object::Reference(_))) {
            let mut stream_length = stream.data.len();
            adjust_aes_stream_length(&mut stream_length, ctx, encrypt_stream_strings)?;
            dict.insert(
                "Length",
                Object::Integer(i64::try_from(stream_length).map_err(|_| {
                    // cov:ignore-start: an allocatable stream length cannot exceed i64::MAX.
                    crate::Error::Unsupported(
                        "encrypted stream /Length does not fit in i64".to_string(),
                    )
                    // cov:ignore-end
                })?), // cov:ignore: llvm-cov attributes this continuation to the impossible overflow arm.
            );
        }
        if let Some(emitter) = encrypted_strings {
            emitter.write_stream_dict(
                buf,
                emitted_ref,
                None,
                &dict,
                encrypted_strings::StreamDictOptions::new(true, false, encrypt_stream_strings),
            )?; // cov:ignore: the encrypted-dictionary route executes; this call continuation has no counter.
        } else {
            dict.write_pdf_stream_qdf(buf, 0);
        }
        let add_newline = write_stream_payload_with_pipeline(
            buf,
            &stream.data,
            policy,
            emitted_ref,
            ctx,
            encrypt_stream_strings,
            None,
        )?; // cov:ignore: the encrypted-payload route executes; this call continuation has no counter.
        Ok(add_newline)
    } else if let Some(emitter) = encrypted_strings {
        emitter.write_stream_dict(
            buf,
            emitted_ref,
            None,
            &stream.dict,
            encrypted_strings::StreamDictOptions::new(true, false, true),
        )?; // cov:ignore: the dictionary-only route executes; this call continuation has no counter.
        write_stream_payload(buf, &stream.data, policy);
        Ok(stream_framing_adds_newline(&stream.data, policy))
    } else {
        stream.dict.write_pdf_stream_qdf(buf, 0);
        write_stream_payload(buf, &stream.data, policy);
        Ok(stream_framing_adds_newline(&stream.data, policy))
    }
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
/// Keys are emitted alphabetically by raw name, **except `/ID` and `/Encrypt`,
/// which are forced last in that order** — this matches qpdf 11.9.0's
/// `QPDFWriter::writeTrailer` (`QPDFWriter.cc:1160-1236`). In QDF mode qpdf
/// writes `/Encrypt` on the same line immediately after `/ID`. Each value is
/// written with the EXISTING compact [`Object::write_pdf`] serializer, so array
/// values such as `/ID [<hex><hex>]` stay inline (qpdf formats the trailer
/// specially). The closing `>>` is followed by a newline; the caller appends
/// `startxref` directly afterwards.
///
/// When `id_writer` is `Some`, the `/ID` *value* is produced by that closure
/// (the `  /ID ` key token is still emitted) instead of serializing the
/// dictionary's stored `/ID` value. This lets the caller compute the `/ID`
/// directly from the bytes written so far — used by the deterministic-`/ID`
/// writer to emit qpdf's content-derived identifier inline rather than via a
/// placeholder-then-patch step. The closure runs only when the `/ID` key is
/// present in the dictionary; if it is absent, `id_writer` is ignored.
#[cfg(test)]
fn write_qdf_trailer(
    bytes: &mut Vec<u8>,
    trailer: &Dictionary,
    id_writer: Option<crate::object::TrailerIdWriter>,
) {
    bytes.extend_from_slice(b"trailer <<\n");

    // `Dictionary::iter()` already yields keys in lexicographic (BTreeMap)
    // order; split out /ID and /Encrypt so they can be appended last in
    // qpdf's writer-state order.
    let mut id_value: Option<&Object> = None;
    let mut encrypt_value: Option<&Object> = None;
    for (key, value) in trailer.iter() {
        if key == b"ID" {
            id_value = Some(value);
            continue;
        }
        if key == b"Encrypt" {
            encrypt_value = Some(value);
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
            None => crate::object::write_id_style_value(bytes, value),
        }
    }
    if let Some(value) = encrypt_value {
        // Encrypted writes always materialize /ID before this serializer;
        // qpdf appends /Encrypt to that same QDF trailer line.
        bytes.extend_from_slice(b" /Encrypt ");
        value.write_pdf(bytes);
    }
    if id_value.is_some() || encrypt_value.is_some() {
        bytes.push(b'\n');
    }

    bytes.extend_from_slice(b">>\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite_renumber::CatalogFirstRenumber;
    use std::io::Cursor;
    use std::sync::Arc;

    struct FinishAfterWriteError {
        finishes: usize,
    }

    impl crate::pipeline::Pipeline for FinishAfterWriteError {
        // cov:ignore-start: run_writer_pipeline never queries test-double identifiers.
        fn identifier(&self) -> &str {
            "finish-after-write-error"
        }
        // cov:ignore-end

        fn write(&mut self, _data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            Err(crate::pipeline::PipelineError::runtime("write failed"))
        }

        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }

    struct FinishErrorPipeline {
        finishes: usize,
    }

    impl crate::pipeline::Pipeline for FinishErrorPipeline {
        // cov:ignore-start: run_writer_pipeline never queries test-double identifiers.
        fn identifier(&self) -> &str {
            "finish-error"
        }
        // cov:ignore-end

        fn write(&mut self, _data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            self.finishes += 1;
            Err(crate::pipeline::PipelineError::runtime("finish failed"))
        }
    }

    #[test]
    fn content_array_holder_collection_uses_one_cumulative_ref_depth_budget() {
        let mut pdf =
            crate::Pdf::open_mem_owned(build_partition_fixture()).expect("fixture must open");
        let refs: Vec<ObjectRef> = (0..=crate::ref_chain::MAX_REF_CHAIN_DEPTH)
            .map(|i| ObjectRef::new(10_000 + i as u32, 0))
            .collect();

        for (index, reference) in refs.iter().enumerate() {
            let value = if let Some(next) = refs.get(index + 1) {
                Object::Reference(*next)
            } else {
                Object::Stream(crate::Stream::new(Dictionary::new(), b"q\rQ\n".to_vec()))
            };
            pdf.set_object(*reference, Object::Array(vec![value]));
        }

        let mut containers = BTreeSet::new();
        collect_content_array_holder_refs(&mut pdf, refs[0], &mut containers)
            .expect("bounded holder traversal must succeed");

        assert_eq!(containers.len(), crate::ref_chain::MAX_REF_CHAIN_DEPTH);
        assert!(containers.contains(&refs[0]));
        assert!(
            !containers.contains(refs.last().expect("non-empty chain")),
            "the terminal holder beyond the shared reference-depth budget must not be visited"
        );
    }

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

    #[test]
    fn remap_qpdf_trailer_refs_propagates_unmapped_nested_live_ref() {
        let fixture = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem_owned(fixture).expect("fixture must open");
        let map = CatalogFirstRenumber::from_pairs_for_test(&[(
            ObjectRef::new(1, 0),
            ObjectRef::new(1, 0),
        )]);
        let mut trailer = Dictionary::new();
        trailer.insert(
            "Extra",
            Object::Array(vec![Object::Reference(ObjectRef::new(3, 0))]),
        );

        let err =
            remap_qpdf_trailer_refs_with_removed(&mut pdf, &mut trailer, &map, &BTreeSet::new())
                .unwrap_err();

        assert!(matches!(err, crate::Error::Unsupported(ref message)
                if message.contains("reference 3 0 R absent from renumber map")));
    }

    #[test]
    fn generate_id_array_static_preserves_id0_when_second_element_is_not_a_string() {
        // `/ID [<valid> 123]` — arity 2 but element 2 is not a string.
        // qpdf's getOriginalID1 reads only element 1, so it remains the
        // permanent ID while element 2 becomes the static changing ID.
        let source = Object::Array(vec![
            Object::String(b"permanent".to_vec()),
            Object::Integer(123),
        ]);
        let v = match generate_id_array(Some(&source), true) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(
            v[0],
            Object::String(b"permanent".to_vec()),
            "qpdf preserves non-empty element 1 without inspecting element 2"
        );
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    #[test]
    fn generate_id_array_static_falls_back_when_id_is_missing() {
        let v = match generate_id_array(None, true) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(v[0], Object::String(QPDF_STATIC_ID.to_vec()));
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    #[test]
    fn generate_id_array_empty_source_uses_same_generated_id() {
        let source = Object::Array(vec![
            Object::String(Vec::new()),
            Object::String(b"source-changing".to_vec()),
        ]);
        let generated = generate_id_array(Some(&source), false);
        let values = match generated {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], values[1]);
        assert!(!str_bytes(&values[0]).is_empty());
        assert_ne!(str_bytes(&values[0]), &QPDF_STATIC_ID[..]);
    }

    #[test]
    fn generate_id_array_empty_source_static_uses_pi_for_both_ids() {
        let source = Object::Array(vec![
            Object::String(Vec::new()),
            Object::String(b"source-changing".to_vec()),
        ]);
        let generated = generate_id_array(Some(&source), true);
        let values = match generated {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(
            values,
            vec![
                Object::String(QPDF_STATIC_ID.to_vec()),
                Object::String(QPDF_STATIC_ID.to_vec()),
            ]
        );
    }

    #[test]
    fn generate_id_array_preserves_non_empty_source_id0() {
        let source = Object::Array(vec![
            Object::String(b"permanent".to_vec()),
            Object::String(b"source-changing".to_vec()),
        ]);
        let generated = generate_id_array(Some(&source), false);
        let values = match generated {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(values[0], Object::String(b"permanent".to_vec()));
        assert!(!str_bytes(&values[1]).is_empty());

        let static_generated = generate_id_array(Some(&source), true);
        let static_values = match static_generated {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(static_values[0], Object::String(b"permanent".to_vec()));
        assert_eq!(static_values[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    // --- Default (no-flag) /ID generation strategy (flpdf-9hc.13.2) ---------

    fn str_bytes(o: &Object) -> &[u8] {
        match o {
            Object::String(s) => s.as_slice(),
            other => panic!("expected /ID element to be a string, got {other:?}"),
        }
    }

    #[test]
    fn generate_id_array_first_save_uses_one_fresh_id_for_both_elements() {
        // No source /ID: qpdf reuses the changing identifier as the permanent
        // identifier, so both elements are equal and independently generated
        // saves still differ.
        let v = match generate_id_array(None, false) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
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
        assert_eq!(e0, e1, "qpdf reuses id2 when source id1 is absent");
    }

    #[test]
    fn generate_id_array_varies_per_save() {
        // Two saves of the same (no-/ID) input yield different /ID arrays —
        // even back-to-back, thanks to the process-global counter.
        let a = generate_id_array(None, false);
        let b = generate_id_array(None, false);
        assert_ne!(a, b, "/ID must change on every save");
    }

    #[test]
    fn generate_id_array_re_save_preserves_element1_rotates_element2() {
        // First save (no source /ID): both fresh.
        let first = match generate_id_array(None, false) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        let v1 = first;
        let perm = v1[0].clone();

        // Re-save: feed the well-formed 2-string /ID back in.  Element 1 must
        // be preserved verbatim (ISO 32000-1 §14.4); element 2 must rotate.
        let source = Object::Array(v1.clone());
        let v2 = match generate_id_array(Some(&source), false) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };

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
    fn generate_id_array_preserves_id0_when_second_element_is_not_a_string() {
        // qpdf's getOriginalID1 reads only non-empty `/ID[0]`; malformed or
        // missing later array elements do not affect the permanent ID.
        let source = Object::Array(vec![
            Object::String(b"would-be-permanent".to_vec()),
            Object::Integer(123),
        ]);
        let v = match generate_id_array(Some(&source), false) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(
            v[0],
            Object::String(b"would-be-permanent".to_vec()),
            "qpdf preserves non-empty element 1 without inspecting element 2"
        );
        assert_eq!(str_bytes(&v[1]).len(), 16);
        assert_ne!(v[0], v[1]);
    }

    #[test]
    fn generate_id_array_static_preserves_singleton_source_id0() {
        // qpdf's getArrayItem(0) needs only the first array item; `/ID[1]`
        // need not exist for a non-empty source permanent identifier to persist.
        let source = Object::Array(vec![Object::String(b"singleton-permanent".to_vec())]);
        let v = match generate_id_array(Some(&source), true) {
            Object::Array(values) => values,
            other => panic!("expected generated /ID array, got {other:?}"), // cov:ignore: test-shape guard
        };
        assert_eq!(v[0], Object::String(b"singleton-permanent".to_vec()));
        assert_eq!(v[1], Object::String(QPDF_STATIC_ID.to_vec()));
    }

    // --- deterministic-id (qpdf --deterministic-id) -----------------------

    fn det_id_options() -> WriterOptions {
        WriterOptions {
            deterministic_id: true,
            ..WriterOptions::default()
        }
    }

    fn write_det_id(fixture: &[u8]) -> Vec<u8> {
        let mut pdf = crate::Pdf::open_mem(Arc::from(fixture)).expect("fixture must open");
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &det_id_options())
            .expect("deterministic-id write must succeed");
        out
    }

    fn trailer_id_pair(output: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let pdf = crate::Pdf::open_mem(Arc::from(output)).expect("output must re-open");
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
    fn ordinary_full_rewrite_empty_source_id0_matches_qpdf_default_and_static() {
        for id_entry in ["/ID [<> <bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb>]", "/ID [() ()]"] {
            let source = build_det_id_source(id_entry, &[]);

            let default_output = write_full_rewrite_with(
                &source,
                &WriterOptions {
                    ..WriterOptions::default()
                },
            );
            let (default_id0, default_id1) = trailer_id_pair(&default_output);
            assert!(!default_id0.is_empty(), "default /ID[0] must be non-empty");
            assert_eq!(default_id0, default_id1);

            let static_output = write_full_rewrite_with(
                &source,
                &WriterOptions {
                    static_id: true,
                    ..WriterOptions::default()
                },
            );
            assert_eq!(
                trailer_id_pair(&static_output),
                (QPDF_STATIC_ID.to_vec(), QPDF_STATIC_ID.to_vec())
            );
        }
    }

    #[test]
    fn encrypted_full_rewrite_empty_source_id0_matches_emitted_id1() {
        let source = build_det_id_source("/ID [<> <bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb>]", &[]);
        let mut pdf = Pdf::open(Cursor::new(source)).expect("source parses");
        let mut output = Vec::new();
        let options = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                Vec::new(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut output, &options).expect("encrypted write");
        let (id0, id1) = trailer_id_pair(&output);
        assert!(!id0.is_empty());
        assert_eq!(
            id0, id1,
            "empty source /ID[0] must reuse qpdf's generated id2"
        );

        let reopened = Pdf::open_with_options(
            Cursor::new(output),
            crate::PdfOpenOptions {
                password: Vec::new(),
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("encrypted output must reopen with the empty user password");
        assert!(reopened.trailer().get("Encrypt").is_some());
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
    fn deterministic_id_preserves_non_16_byte_source_id() {
        // qpdf's getOriginalID1 preserves /ID[0] verbatim regardless of length;
        // only /ID[1] is regenerated (always a 16-byte md5).
        let src = build_det_id_source(
            &format!("/ID [<{}><{}>]", "aa".repeat(20), "bb".repeat(16)),
            &[],
        );
        let out = write_det_id(&src);
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(
            id0,
            vec![0xAAu8; 20],
            "/ID[0] must be preserved verbatim (20 bytes)"
        );
        assert_eq!(id1.len(), 16, "/ID[1] is always a 16-byte digest");
        assert_eq!(id1, expected_changing_id(&out, b"").to_vec());
        assert_ne!(id0, id1);
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
    fn deterministic_id_treats_empty_source_id0_as_absent() {
        // An empty source /ID[0] (`<>`) is not a usable permanent identifier:
        // qpdf falls back to the generated changing identifier (verified against
        // qpdf 11.9.0, where /ID[0] == /ID[1] for an empty original id). It must
        // NOT be preserved verbatim as an empty string.
        let src = build_det_id_source(&format!("/ID [<><{}>]", "bb".repeat(16)), &[]);
        let out = write_det_id(&src);
        let (id0, id1) = trailer_id_pair(&out);
        assert_eq!(id0, id1, "empty source /ID[0] must fall back to /ID[1]");
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
        let (id0, id1) =
            compute_deterministic_id(&legacy_buf, id_array_offset, b"", Some(src0.as_slice()));
        let mut expect = prefix.clone();
        write_deterministic_id_array(&mut expect, &src0, &id1);

        let mut inline = prefix.clone();
        write_deterministic_id_inline(&mut inline, b"", Some(src0.as_slice()));

        assert_eq!(
            inline, expect,
            "inline write with a source id must equal placeholder+patch output"
        );
        assert_eq!(
            id0,
            src0.to_vec(),
            "/ID[0] must be the supplied permanent identifier"
        );
        assert_ne!(
            id0, id1,
            "permanent and changing identifiers must differ in general"
        );
    }

    #[test]
    fn write_pdf_with_id_writer_none_emits_qpdf_trailer_id_shape() {
        // With `id_writer = None`, the fallback routes the stored `/ID` value
        // through `write_id_style_value` to reproduce qpdf's `writeTrailer`
        // compact `[<hex1><hex2>]` (no separating spaces). Plain `write_pdf`
        // goes through the generic array serializer, which now — matching
        // qpdf's `unparseObject` — inserts spaces on both sides of every
        // element (`[ <hex1> <hex2> ]`). Pin both shapes: /ID via the None
        // fallback stays qpdf-trailer-compact; other keys agree byte-for-byte
        // with plain `write_pdf`.
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

        let mut via_none = Vec::new();
        dict.write_pdf_with_id_writer(&mut via_none, None);
        // Render once so the diagnostic bytes appear on the covered path too;
        // avoids a coverage hole in the assert-message arm that only runs on
        // failure.
        let via_none_str = String::from_utf8_lossy(&via_none).into_owned();
        // The compact shape puts `[` immediately before `<`, no separator.
        assert!(
            via_none.windows(6).any(|w| w == b"/ID [<"),
            "None fallback must emit compact `/ID [<...` without space between `[` and `<`; got: {via_none_str}"
        );
        assert!(
            !via_none.windows(8).any(|w| w == b"/ID [ <"),
            "None fallback must NOT emit `/ID [ <...` (that is the generic array serializer); got: {via_none_str}"
        );

        // Plain `write_pdf` now goes through the generic array serializer, so
        // its /ID output is space-separated. Prove the two paths intentionally
        // diverge on the /ID value (they must not silently drift back to the
        // pre-fix "both use write_pdf" behavior).
        let mut plain = Vec::new();
        dict.write_pdf(&mut plain);
        let plain_str = String::from_utf8_lossy(&plain).into_owned();
        assert!(
            plain.windows(7).any(|w| w == b"/ID [ <"),
            "plain write_pdf must emit the generic `/ID [ <...` shape; got: {plain_str}"
        );
        assert_ne!(
            via_none, plain,
            "None fallback (qpdf-trailer /ID shape) must differ from plain write_pdf (generic array /ID shape)"
        );
    }

    #[test]
    fn write_pdf_with_id_writer_none_matches_write_pdf_without_id() {
        // Without an `/ID` key, the None fallback has no ID-value substitution
        // to perform, so it must remain byte-identical to plain `write_pdf`.
        let mut dict = Dictionary::new();
        dict.insert("Size", Object::Integer(4));
        dict.insert("Root", Object::reference(ObjectRef::new(1, 0)));

        let mut plain = Vec::new();
        dict.write_pdf(&mut plain);
        let mut via_none = Vec::new();
        dict.write_pdf_with_id_writer(&mut via_none, None);
        assert_eq!(
            via_none, plain,
            "write_pdf_with_id_writer(None) must equal write_pdf when no /ID is present"
        );
    }

    #[test]
    fn write_stream_to_buf_with_id_writer_none_emits_qpdf_trailer_id_shape() {
        // The xref-stream helper's `None` fallback likewise routes stored `/ID`
        // through `write_id_style_value`, so the payload framing agrees with
        // `write_stream_to_buf` on everything except the /ID value shape. Only
        // production paths passing `Some` are used in practice, but pin the
        // `None` contract: /ID is compact, other bytes match the plain helper.
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

        let mut actual = Vec::new();
        write_stream_to_buf_with_id_writer(&mut actual, &stream, NewlineBeforeEndstream::Yes, None);
        let actual_str = String::from_utf8_lossy(&actual).into_owned();
        assert!(
            actual.windows(6).any(|w| w == b"/ID [<"),
            "None fallback must emit compact `/ID [<...` (no space); got: {actual_str}"
        );
        assert!(
            !actual.windows(7).any(|w| w == b"/ID [ <"),
            "None fallback must not emit the generic space-separated `/ID [ <...`; got: {actual_str}"
        );
    }

    #[test]
    fn write_stream_to_buf_with_id_writer_none_matches_helper_without_id() {
        // Without `/ID` in the dictionary, the xref-stream helper's `None`
        // fallback is byte-identical to `write_stream_to_buf`.
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::Integer(3));
        let stream = crate::Stream::new(dict, b"abc".to_vec());

        let mut expected = Vec::new();
        write_stream_to_buf(&mut expected, &stream, NewlineBeforeEndstream::Yes);
        let mut actual = Vec::new();
        write_stream_to_buf_with_id_writer(&mut actual, &stream, NewlineBeforeEndstream::Yes, None);
        assert_eq!(
            actual, expected,
            "None fallback must equal write_stream_to_buf when no /ID is present"
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
        let reopened = crate::Pdf::open_mem(Arc::from(&out[..])).expect("output must re-open");
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
        let reopened = crate::Pdf::open_mem(Arc::from(&out[..])).expect("output must re-open");
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
        let opts = WriterOptions {
            deterministic_id: true,
            qdf: true,
            ..WriterOptions::default()
        };
        let mut pdf = crate::Pdf::open_mem(Arc::from(fixture)).expect("fixture must open");
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &opts).expect("qdf deterministic write");
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
        let reopened = crate::Pdf::open_mem(Arc::from(&out[..])).expect("output must re-open");
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
        let opts = WriterOptions {
            qdf: true,
            static_id: true,
            ..WriterOptions::default()
        };
        let write = |f: &[u8]| {
            let mut pdf = crate::Pdf::open_mem(Arc::from(f)).expect("fixture must open");
            let mut out = Vec::new();
            emit_canonical_pdf(&mut pdf, &mut out, &opts).expect("qdf write");
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
    fn qdf_encrypted_trailer_puts_encrypt_after_id_on_the_same_final_line() {
        let mut trailer = Dictionary::new();
        trailer.insert("Encrypt", Object::Reference(ObjectRef::new(9, 0)));
        trailer.insert("Info", Object::Reference(ObjectRef::new(7, 0)));
        trailer.insert("Root", Object::Reference(ObjectRef::new(1, 0)));
        trailer.insert("Size", Object::Integer(10));
        trailer.insert(
            "ID",
            Object::Array(vec![Object::String(vec![1]), Object::String(vec![2])]),
        );

        let mut out = Vec::new();
        write_qdf_trailer(&mut out, &trailer, None);

        assert_eq!(
            out,
            b"trailer <<\n  /Info 7 0 R\n  /Root 1 0 R\n  /Size 10\n  /ID [<01><02>] /Encrypt 9 0 R\n>>\n",
            "qpdf writes normal keys first, followed by /ID and /Encrypt on the final line"
        );
    }

    #[test]
    fn qdf_trailer_without_id_or_encrypt_closes_after_sorted_keys() {
        let mut trailer = Dictionary::new();
        trailer.insert("Root", Object::Reference(ObjectRef::new(1, 0)));
        trailer.insert("Size", Object::Integer(2));

        let mut out = Vec::new();
        write_qdf_trailer(&mut out, &trailer, None);

        assert_eq!(out, b"trailer <<\n  /Root 1 0 R\n  /Size 2\n>>\n");
    }

    #[test]
    fn deterministic_id_xref_stream_is_self_stable() {
        // xref-stream form: qpdf does not produce byte-parity here, but the
        // content-derived /ID must still be deterministic (self-stable) and the
        // /ID[1] must match the two-level reconstruction.
        let fixture = build_partition_fixture();
        let opts = WriterOptions {
            deterministic_id: true,
            // Generate ObjStm batches so the writer emits cross-reference
            // stream form rather than a classic xref table.
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let write = |f: &[u8]| {
            let mut pdf = crate::Pdf::open_mem(Arc::from(f)).expect("fixture must open");
            let mut out = Vec::new();
            emit_canonical_pdf(&mut pdf, &mut out, &opts).expect("write");
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

    /// Regression: the xref-stream trailer's `/ID` must use qpdf's compact
    /// `[<hex1><hex2>]` shape (no separating spaces) even when the run is
    /// **non-deterministic** — e.g. `--static-id` combined with a
    /// generate-mode plan that upgrades to xref-stream form. Before the
    /// `write_stream_to_buf_with_id_writer(_, None)` routing, this branch
    /// serialized the xref-stream dict via plain `write_stream_to_buf`,
    /// which now goes through the generic array serializer and would emit
    /// `/ID [ <hex1> <hex2> ]` (space-separated). qpdf's own hand-rolled
    /// trailer / linearization `xref_stream::write_object` path always
    /// emits the compact shape, so drifting to spaces would be a silent
    /// parity regression.
    #[test]
    fn static_id_xref_stream_emits_qpdf_compact_id_shape() {
        let fixture = build_partition_fixture();
        let opts = WriterOptions {
            static_id: true,
            // Force ObjStm batches → XrefForm::Stream → the `else` branch of
            // the xref-stream writer (`deterministic_id` is off, `static_id`
            // is on) is the one Codex flagged.
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let mut pdf = crate::Pdf::open_mem_owned(fixture).expect("fixture must open");
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &opts).expect("write");

        // The compact shape places `[` immediately before `<` (no space).
        // The generic array serializer would emit `[ <` — the regression.
        // Materialize the diagnostic on the covered path so the assert-message
        // arm is not a coverage hole.
        let out_str = String::from_utf8_lossy(&out).into_owned();
        assert!(
            out.windows(6).any(|w| w == b"/ID [<"),
            "xref-stream trailer /ID must be compact `/ID [<...` (qpdf shape); \
             got: {out_str}"
        );
        assert!(
            !out.windows(7).any(|w| w == b"/ID [ <"),
            "xref-stream trailer /ID must NOT drift to `/ID [ <...` (that is \
             the generic array serializer, which regresses qpdf parity); \
             got: {out_str}"
        );
    }

    #[test]
    fn deterministic_id_preserve_xref_stream_form_is_self_stable() {
        // A cross-reference-stream INPUT written with --object-streams=preserve
        // keeps Stream form even with no surviving ObjStm batches. The
        // deterministic `/ID` is direct-written inline at the xref-stream dict's
        // sorted `/ID` position;
        // qpdf does not byte-match xref-stream form here, but the identifier must
        // be self-stable and `/ID[1]` must equal the two-level reconstruction.
        let fixture = build_xref_stream_fixture();
        let opts = WriterOptions {
            deterministic_id: true,
            object_streams: ObjectStreamMode::Preserve,
            ..WriterOptions::default()
        };
        let write = |f: &[u8]| {
            let mut pdf =
                crate::Pdf::open_mem(Arc::from(f)).expect("xref-stream fixture must open");
            let mut out = Vec::new();
            emit_canonical_pdf(&mut pdf, &mut out, &opts).expect("write");
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
        let mut pdf = crate::Pdf::open_mem_owned(fixture).expect("fixture must open");
        let opts = WriterOptions {
            deterministic_id: true,
            static_id: true,
            ..WriterOptions::default()
        };
        let err = emit_canonical_pdf(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("mutually exclusive")),
            "got {err:?}"
        );
    }

    #[test]
    fn encrypt_and_copy_encryption_are_mutually_exclusive() {
        let fixture = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem_owned(fixture).expect("fixture must open");
        let opts = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            copy_encryption: Some(crate::encrypt_setup::CopyEncryptionSource {
                encrypt_dict: Dictionary::new(),
                file_key: Vec::new(),
                id0: Vec::new(),
                object_key_alg: crate::ObjectKeyAlg::Aes,
            }),
            ..WriterOptions::default()
        };
        let err = emit_canonical_pdf(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m)
                if m == "encrypt and copy_encryption are mutually exclusive"),
            "got {err:?}"
        );
    }

    #[test]
    fn deterministic_id_rejected_with_encryption() {
        let fixture = build_partition_fixture();
        let mut pdf = crate::Pdf::open_mem_owned(fixture).expect("fixture must open");
        let opts = WriterOptions {
            deterministic_id: true,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let err = emit_canonical_pdf(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m)
                if m == "the deterministic-id option is incompatible with encrypted output files"),
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

    // --- xref-stream fixture helpers ----------------------------------------

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

    // ── static_aes_iv tests (flpdf-9hc.4.13) ─────────────────────────────────

    /// Build an encrypted PDF with `static_aes_iv = true` and verify that
    /// every AES IV (the first 16 bytes of each ciphertext block) is all-zero.
    ///
    /// We also set `static_id = true` so that the file key — derived from
    /// `/ID[0]` — is deterministic; without that the file key itself changes
    /// between runs and the stream IV bytes would vary regardless.
    #[test]
    fn static_aes_iv_uses_the_vector_qpdf_writes_for_streams_and_strings() {
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
        let options = WriterOptions {
            static_id: true,
            static_aes_iv: true,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("encrypted write");

        // The output must be deterministic: running again with the same options
        // must produce byte-identical bytes.
        let mut pdf2 = Pdf::open(Cursor::new(fixture.clone())).unwrap();
        let mut out2 = Vec::new();
        emit_canonical_pdf(&mut pdf2, &mut out2, &options).expect("encrypted write 2");
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
        // AES-encrypted content/metadata stream whose IV must be qpdf's.
        //
        // The vector is qpdf's `14 * (1 + i)` (`libqpdf/Pl_AES_PDF.cc:136-139`),
        // not zeros: `--static-aes-iv` exists so that output is comparable with
        // qpdf's, and CBC writes the vector into the file, so a different
        // vector means a different file. An earlier revision of this test
        // asserted zeros, which pinned flpdf against itself.
        let expected = crate::pipeline::aes::static_initialization_vector();
        const NEEDLE: &[u8] = b"\nstream\n";
        let mut checked = 0usize;
        let mut pos = 0usize;
        while let Some(rel) = out[pos..].windows(NEEDLE.len()).position(|w| w == NEEDLE) {
            let payload = pos + rel + NEEDLE.len();
            let iv = &out[payload..payload + 16];
            assert_eq!(
                iv,
                &expected,
                "static_aes_iv: stream payload at byte {payload} must begin with qpdf's AES IV, got {iv:02x?}"
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
            let options = WriterOptions {
                encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                    b"user".to_vec(),
                    b"owner".to_vec(),
                )),
                ..WriterOptions::default()
            };
            emit_canonical_pdf(&mut pdf, &mut out, &options).unwrap();
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

    /// Copy-encryption is supported in QDF mode through both the library and
    /// CLI. QDF inserts an indirect
    /// `/Length` holder after each stream, so `/Encrypt` must be allocated
    /// after the final interleaved holder rather than after the last source
    /// object. Re-opening the result exercises that non-colliding allocation.
    #[test]
    fn qdf_copy_encryption_allocates_encrypt_after_length_holders() {
        use crate::encrypt_setup::{CopyEncryptionSource, EncryptParams};
        use crate::security::standard::ObjectKeyAlg;
        use crate::PdfOpenOptions;
        use std::io::Cursor;

        let derivation_options = WriterOptions {
            static_aes_iv: true,
            ..WriterOptions::default()
        };
        let params = EncryptParams::v4_aes128(b"user-pw".to_vec(), b"owner-pw".to_vec());
        let donor =
            build_encryption_context(&derivation_options, &params, 4, None, &QPDF_STATIC_ID)
                .expect("derive deterministic donor encryption state");
        let source = CopyEncryptionSource {
            encrypt_dict: donor.encrypt_dict,
            file_key: donor.file_key,
            id0: donor.id0,
            object_key_alg: ObjectKeyAlg::Aes,
        };

        let fixture = build_string_and_stream_fixture();
        let mut pdf = Pdf::open(Cursor::new(fixture)).expect("open fixture");
        let mut out = Vec::new();
        let options = WriterOptions {
            qdf: true,
            compress_streams: CompressStreams::No,
            static_aes_iv: true,
            copy_encryption: Some(source),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut out, &options)
            .expect("QDF copy-encryption write must succeed");
        assert!(out.windows(b"%QDF-1.0".len()).any(|w| w == b"%QDF-1.0"));

        let mut reopened = Pdf::open_with_options(
            Cursor::new(out),
            PdfOpenOptions {
                password: b"user-pw".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("QDF copy-encryption output must reopen with donor password");
        let encrypt_ref = reopened
            .trailer()
            .get_ref("Encrypt")
            .expect("trailer must reference /Encrypt");
        let root_ref = reopened.root_ref().expect("root_ref");
        let root = reopened.resolve(root_ref).expect("resolve /Root");
        let metadata_ref = root
            .as_dict()
            .and_then(|dict| dict.get_ref("Metadata"))
            .expect("Catalog must reference /Metadata");
        let metadata = reopened.resolve(metadata_ref).expect("resolve /Metadata");
        let length_ref = metadata
            .as_stream()
            .and_then(|stream| stream.dict.get_ref("Length"))
            .expect("QDF stream must use an indirect /Length holder");
        assert_ne!(
            encrypt_ref, length_ref,
            "/Encrypt must not collide with QDF's interleaved /Length holder"
        );
        let (title, stream) = resolve_title_and_stream(&mut reopened);
        assert_eq!(title, b"TopSecretTitle");
        assert_eq!(stream, b"hello");
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

    #[test]
    fn v5_randomness_from_qpdf_order_maps_fixed_input() {
        let bytes = std::array::from_fn(|index| index as u8);
        let randomness = V5Randomness::from_bytes(bytes);

        assert_eq!(
            randomness.file_key,
            std::array::from_fn(|index| index as u8)
        );
        assert_eq!(
            randomness.user_validation_salt,
            std::array::from_fn(|index| 32 + index as u8)
        );
        assert_eq!(
            randomness.user_key_salt,
            std::array::from_fn(|index| 40 + index as u8)
        );
        assert_eq!(
            randomness.owner_validation_salt,
            std::array::from_fn(|index| 48 + index as u8)
        );
        assert_eq!(
            randomness.owner_key_salt,
            std::array::from_fn(|index| 56 + index as u8)
        );
        assert_eq!(
            randomness.perms_random_tail,
            std::array::from_fn(|index| 64 + index as u8)
        );
    }

    fn fixed_v5_encrypted_output(
        params: crate::encrypt_setup::EncryptParams,
        allow_weak_crypto: bool,
    ) -> Vec<u8> {
        use crate::PdfOpenOptions;
        use std::io::Cursor;

        let fixture = build_string_and_stream_fixture();
        let mut pdf = Pdf::open(Cursor::new(fixture)).expect("open fixture");
        let mut out = Vec::new();
        let options = WriterOptions {
            static_id: true,
            static_aes_iv: true,
            compress_streams: CompressStreams::No,
            encrypt: Some(params),
            v5_randomness: Some(V5Randomness::from_bytes(std::array::from_fn(|index| {
                0x80u8.wrapping_add(index as u8)
            }))),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("V=5 encrypted write");

        let mut reopened = Pdf::open_with_options(
            Cursor::new(out.clone()),
            PdfOpenOptions {
                password: b"user-pw".to_vec(),
                allow_weak_crypto,
                ..PdfOpenOptions::default()
            },
        )
        .expect("fixed V=5 output must authenticate");
        let (title, stream) = resolve_title_and_stream(&mut reopened);
        assert_eq!(title, b"TopSecretTitle");
        assert_eq!(stream, b"hello");
        out
    }

    #[test]
    fn v5_r6_fixed_randomness_is_byte_stable() {
        let first = fixed_v5_encrypted_output(
            crate::encrypt_setup::EncryptParams::v5_r6(b"user-pw".to_vec(), b"owner-pw".to_vec()),
            false,
        );
        let second = fixed_v5_encrypted_output(
            crate::encrypt_setup::EncryptParams::v5_r6(b"user-pw".to_vec(), b"owner-pw".to_vec()),
            false,
        );
        assert_eq!(
            first, second,
            "fixed V=5 R=6 input must produce stable bytes"
        );
    }

    #[test]
    fn v5_r5_fixed_randomness_is_byte_stable() {
        let first = fixed_v5_encrypted_output(
            crate::encrypt_setup::EncryptParams::v5_r5(b"user-pw".to_vec(), b"owner-pw".to_vec()),
            true,
        );
        let second = fixed_v5_encrypted_output(
            crate::encrypt_setup::EncryptParams::v5_r5(b"user-pw".to_vec(), b"owner-pw".to_vec()),
            true,
        );
        assert_eq!(
            first, second,
            "fixed V=5 R=5 input must produce stable bytes"
        );
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
        let options = WriterOptions {
            // Keep the stream uncompressed so the decrypted payload is exactly
            // the original bytes (no filter round-trip to account for).
            compress_streams: CompressStreams::No,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v5_r6(
                b"user-pw".to_vec(),
                b"owner-pw".to_vec(),
            )),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("V=5 R=6 encrypted write");

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
        let options = WriterOptions {
            // Keep the stream uncompressed so the decrypted payload is exactly
            // the original bytes (no filter round-trip to account for).
            compress_streams: CompressStreams::No,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v5_r5(
                b"user-pw".to_vec(),
                b"owner-pw".to_vec(),
            )),
            ..WriterOptions::default()
        };
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("V=5 R=5 encrypted write");

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

    #[test]
    fn aes_stream_pipeline_reports_invalid_key_as_internal_error() {
        let object_ref = ObjectRef::new(7, 0);
        let ctx = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 31],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut output = Vec::new();
        let error = pipe_writer_stream_payload(
            &mut output,
            b"payload",
            object_ref,
            &ctx,
            true,
            Some([0u8; 16]),
        )
        .expect_err("invalid AES key length must fail before plaintext fallback");

        assert!(
            matches!(error, crate::Error::Internal(message) if message.contains("key must be 16 or 32 bytes"))
        );
    }

    #[test]
    fn adjust_aes_stream_length_matches_qpdf_formula_and_gates() {
        let context = |cipher| EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 32],
            cipher,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };

        let mut aes_length = 82;
        adjust_aes_stream_length(&mut aes_length, &context(WriteCipher::FileKeyAes256), true)
            .expect("AES length adjustment");
        assert_eq!(aes_length, 112, "qpdf's 82-byte AES probe grows to 112");

        let mut rc4_length = 82;
        adjust_aes_stream_length(
            &mut rc4_length,
            &context(WriteCipher::PerObject(
                crate::security::standard::ObjectKeyAlg::Rc4,
            )),
            true,
        )
        .expect("RC4 length check");
        assert_eq!(rc4_length, 82, "RC4 has no IV or block padding");

        let mut cleartext_length = 82;
        adjust_aes_stream_length(
            &mut cleartext_length,
            &context(WriteCipher::FileKeyAes256),
            false,
        )
        .expect("cleartext length check");
        assert_eq!(cleartext_length, 82, "cleartext metadata stays raw");

        let mut empty_key_length = 82;
        let mut empty_key_context = context(WriteCipher::FileKeyAes256);
        empty_key_context.file_key.clear();
        adjust_aes_stream_length(&mut empty_key_length, &empty_key_context, true)
            .expect("empty-key length check");
        assert_eq!(
            empty_key_length, 82,
            "an empty V=5 key has no current stage"
        );
    }

    #[test]
    fn writer_pipeline_without_stage_finishes_empty_payload() {
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 16],
            cipher: WriteCipher::PerObject(crate::security::standard::ObjectKeyAlg::Rc4),
            encryption_v: 1,
            encryption_r: 2,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: false,
            encrypt_metadata: false,
            metadata_ref: Some(ObjectRef::new(4, 0)),
        };
        let mut output = Vec::new();

        let last_byte = pipe_writer_stream_payload(
            &mut output,
            &[],
            ObjectRef::new(4, 0),
            &context,
            false,
            None,
        )
        .expect("stage-free pipeline");

        assert!(output.is_empty());
        assert_eq!(
            last_byte, 0,
            "Count starts at qpdf's empty-payload sentinel"
        );
    }

    #[test]
    fn writer_pipeline_with_empty_key_skips_encryption_stage() {
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: Vec::new(),
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut output = Vec::new();

        pipe_writer_stream_payload(
            &mut output,
            b"empty-key payload",
            ObjectRef::new(7, 0),
            &context,
            true,
            Some([0u8; 16]),
        )
        .expect("empty current key must use the active pipeline without a stage");

        assert_eq!(output, b"empty-key payload");
    }

    #[test]
    fn aes_writer_pipeline_matches_block_boundary_lengths() {
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 32],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };

        for (plain_length, expected_wire_length) in [
            (0, 32),
            (1, 32),
            (15, 32),
            (16, 48),
            (17, 48),
            (31, 48),
            (32, 64),
        ] {
            let mut output = Vec::new();
            pipe_writer_stream_payload(
                &mut output,
                &vec![0x41; plain_length],
                ObjectRef::new(7, 0),
                &context,
                true,
                Some([0u8; 16]),
            )
            .expect("AES block-boundary payload");
            assert_eq!(
                output.len(),
                expected_wire_length,
                "AES wire length for {plain_length} plaintext bytes"
            );
        }
    }

    #[test]
    fn aes_writer_pipeline_generates_random_iv_and_honors_yes_newline() {
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 32],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: false,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut output = Vec::new();

        let added_newline = write_stream_payload_with_pipeline(
            &mut output,
            b"random-IV payload",
            NewlineBeforeEndstream::Yes,
            ObjectRef::new(7, 0),
            &context,
            true,
            None,
        )
        .expect("AES pipeline with an OS-generated IV");

        assert!(added_newline);
        assert!(output.starts_with(b"\nstream\n"));
        assert!(output.ends_with(b"\nendstream"));
    }

    #[test]
    fn write_reencoded_object_covers_encrypted_stream_routes() {
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 32],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut dict = Dictionary::new();
        dict.insert("Label", Object::String(b"reencoded label".to_vec()));
        dict.insert("Length", Object::Integer(7));
        let stream = Object::Stream(crate::Stream::new(dict, b"payload".to_vec()));
        let options = WriterOptions {
            compress_streams: CompressStreams::No,
            ..WriterOptions::default()
        };
        let emitted_ref = ObjectRef::new(7, 0);

        let mut plain_dict_bytes = Vec::new();
        write_reencoded_object(
            &mut plain_dict_bytes,
            &stream,
            false,
            &options,
            None,
            emitted_ref,
            StreamEncryptionOptions::new(Some(&context), true),
        )
        .expect("encrypted stream with a plain dictionary emitter");
        assert!(plain_dict_bytes.ends_with(b"endstream"));

        let mut emitter = encrypted_strings::EncryptedStringEmitter::from_context(&context);
        let mut encrypted_dict_bytes = Vec::new();
        write_reencoded_object(
            &mut encrypted_dict_bytes,
            &stream,
            false,
            &options,
            Some(&mut emitter),
            emitted_ref,
            StreamEncryptionOptions::new(Some(&context), true),
        )
        .expect("encrypted stream with an encrypted dictionary emitter");
        assert!(encrypted_dict_bytes.ends_with(b"endstream"));

        let mut emitter = encrypted_strings::EncryptedStringEmitter::from_context(&context);
        let mut encrypted_only_dict_bytes = Vec::new();
        write_reencoded_object(
            &mut encrypted_only_dict_bytes,
            &stream,
            false,
            &options,
            Some(&mut emitter),
            emitted_ref,
            StreamEncryptionOptions::new(None, true),
        )
        .expect("stream with an encrypted dictionary and plain payload");
        assert!(encrypted_only_dict_bytes.ends_with(b"endstream"));
    }

    #[test]
    fn write_stream_to_buf_qdf_covers_encrypted_and_plain_routes() {
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 32],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut dict = Dictionary::new();
        dict.insert("Label", Object::String(b"qdf label".to_vec()));
        let stream = crate::Stream::new(dict, b"payload".to_vec());
        let emitted_ref = ObjectRef::new(7, 0);

        let mut out = Vec::new();
        write_stream_to_buf_qdf(
            &mut out,
            &stream,
            NewlineBeforeEndstream::No,
            None,
            emitted_ref,
            Some(&context),
            true,
        )
        .expect("QDF encrypted payload with a plain dictionary");
        assert!(out.ends_with(b"endstream"));

        let mut emitter = encrypted_strings::EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();
        write_stream_to_buf_qdf(
            &mut out,
            &stream,
            NewlineBeforeEndstream::No,
            Some(&mut emitter),
            emitted_ref,
            Some(&context),
            true,
        )
        .expect("QDF encrypted payload and dictionary");
        assert!(out.ends_with(b"endstream"));

        let mut emitter = encrypted_strings::EncryptedStringEmitter::from_context(&context);
        let mut out = Vec::new();
        write_stream_to_buf_qdf(
            &mut out,
            &stream,
            NewlineBeforeEndstream::No,
            Some(&mut emitter),
            emitted_ref,
            None,
            true,
        )
        .expect("QDF encrypted dictionary with a plain payload");
        assert!(out.ends_with(b"endstream"));

        let mut out = Vec::new();
        write_stream_to_buf_qdf(
            &mut out,
            &stream,
            NewlineBeforeEndstream::No,
            None,
            emitted_ref,
            None,
            false,
        )
        .expect("plain QDF stream");
        assert!(out.ends_with(b"endstream"));
    }

    #[test]
    fn writer_pipeline_finishes_after_downstream_write_error() {
        let mut pipeline = FinishAfterWriteError { finishes: 0 };
        let error = run_writer_pipeline(&mut pipeline, b"payload")
            .expect_err("downstream write failure must propagate");

        assert!(matches!(error, crate::Error::System(message) if message == "write failed"));
        assert_eq!(pipeline.finishes, 1, "finish must balance a failed write");
    }

    #[test]
    fn writer_pipeline_propagates_downstream_finish_error() {
        let mut pipeline = FinishErrorPipeline { finishes: 0 };
        let error = run_writer_pipeline(&mut pipeline, b"payload")
            .expect_err("downstream finish failure must propagate");

        assert!(matches!(error, crate::Error::System(message) if message == "finish failed"));
        assert_eq!(pipeline.finishes, 1);
    }

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
            let options = WriterOptions {
                // Keep the stream uncompressed so the decrypted payload equals
                // the original bytes.
                compress_streams: CompressStreams::No,
                encrypt: Some(EncryptParams::rc4(
                    method,
                    b"user-pw".to_vec(),
                    b"owner-pw".to_vec(),
                )),
                ..WriterOptions::default()
            };
            emit_canonical_pdf(&mut pdf, &mut out, &options)
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

    /// V=4 encryption requires a PDF header floored per qpdf's method-specific
    /// table (QPDFWriter.cc L810): AES → 1.6, RC4 → 1.5. Prior to the
    /// encryption-floor fix flpdf floored both to 1.5, which was correct for
    /// RC4 but under-shot AES; this test verifies each method emits the
    /// qpdf-equivalent floor for a 1.4 input.
    #[test]
    fn v4_encryption_floors_pdf_header_per_qpdf_table() {
        use crate::encrypt_setup::{EncryptMethod, EncryptParams};
        use std::io::Cursor;

        let fixture = build_partition_fixture();
        assert!(
            fixture.starts_with(b"%PDF-1.4"),
            "fixture must start at %PDF-1.4 for this test to be meaningful"
        );

        for (params, expected_prefix) in [
            (
                EncryptParams::v4_aes128(b"u".to_vec(), b"o".to_vec()),
                b"%PDF-1.6".as_slice(),
            ),
            (
                EncryptParams::rc4(EncryptMethod::V4Rc4128, b"u".to_vec(), b"o".to_vec()),
                b"%PDF-1.5".as_slice(),
            ),
        ] {
            let method = params.method;
            let mut pdf = Pdf::open(Cursor::new(fixture.clone())).unwrap();
            let mut out = Vec::new();
            let options = WriterOptions {
                encrypt: Some(params),
                ..WriterOptions::default()
            };
            emit_canonical_pdf(&mut pdf, &mut out, &options).expect("V=4 encrypted write");
            // cov:ignore-start: multi-line assert; llvm-cov attributes only the
            // "on-panic" format-argument evaluations to the outer line, and the
            // assertion succeeds so those are never evaluated.
            assert!(
                out.starts_with(expected_prefix),
                "{method:?} must floor the header to {}, got {:?}",
                String::from_utf8_lossy(expected_prefix),
                String::from_utf8_lossy(&out[..out.len().min(12)])
            );
            // cov:ignore-end
        }
    }

    /// A malformed source header is preserved by `effective_pdf_version`, so
    /// the full-rewrite encryption floor remains responsible for repairing the
    /// emitted header. Keep this recovery path covered while version handling
    /// is routed through `PdfVersion`.
    #[test]
    fn encryption_repairs_unparseable_source_header() {
        use crate::encrypt_setup::EncryptParams;
        use std::io::Cursor;

        let mut fixture = build_partition_fixture();
        assert!(fixture.starts_with(b"%PDF-1.4"));
        fixture[..8].copy_from_slice(b"%PDF-x.y");

        let mut pdf = Pdf::open(Cursor::new(fixture)).expect("open malformed-version fixture");
        let mut out = Vec::new();
        let options = WriterOptions {
            encrypt: Some(EncryptParams::v5_r6(b"u".to_vec(), b"o".to_vec())),
            ..WriterOptions::default()
        };

        emit_canonical_pdf(&mut pdf, &mut out, &options)
            .expect("encrypted full rewrite repairs the header");
        assert!(out.starts_with(b"%PDF-1.7"));
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
    /// without a `/Crypt` filter, and the `/Encrypt` dict carries
    /// `/EncryptMetadata false` — whereas the default (`encrypt_metadata = true`)
    /// ciphers it.
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
            let options = WriterOptions {
                // No compression so cleartext metadata is a literal substring.
                compress_streams: CompressStreams::No,
                encrypt: Some(params),
                ..WriterOptions::default()
            };
            emit_canonical_pdf(&mut pdf, &mut out, &options).expect("encrypted write");
            out
        };

        // Default: /Metadata is encrypted, so the marker does not appear.
        let enc = encrypt(true);
        assert!(
            !contains(&enc, MARKER),
            "default encrypt must cipher the /Metadata stream"
        );

        // --cleartext-metadata: the XMP marker survives in the clear and the
        // dict emits /EncryptMetadata false without adding a /Crypt filter.
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
            !contains(&ct, b"/Crypt"),
            "cleartext /Metadata must not be tagged with a /Crypt filter"
        );
    }

    #[test]
    fn write_options_default_min_extension_level_is_none() {
        let options = WriterOptions::default();
        assert!(options.min_extension_level.is_none());
    }

    #[test]
    fn write_options_min_extension_level_stores_and_returns_value() {
        let options = WriterOptions {
            min_extension_level: Some(8),
            ..Default::default()
        };
        assert_eq!(options.min_extension_level, Some(8));
    }

    #[test]
    fn effective_pdf_version_and_ext_no_bump_when_min_unset() {
        // Source 1.7 ext 0, options empty → (1.7, 0).
        let options = WriterOptions::default();
        let (v, e) = effective_pdf_version_and_ext("1.7", 0, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_pairwise_min_version_bump_resets_ext_to_source() {
        // Source (1.3, 0), min_version=1.7, min_ext=Some(0) is illegal but None is the
        // typical case here → min wins by version → ext resets to source_ext (0).
        let options = WriterOptions {
            min_version: Some("1.7".into()),
            min_extension_level: None,
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.3", 0, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_min_ext_wins_when_versions_equal() {
        // Source (1.7, 0), min (1.7, 8) → tie on ver, min_ext wins → (1.7, 8).
        let options = WriterOptions {
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 0, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 8);
    }

    #[test]
    fn effective_pdf_version_and_ext_source_ver_higher_resets_min_ext() {
        // Source (2.0, 0), min (1.7, 8) → source wins by ver → ext resets to source_ext (0).
        let options = WriterOptions {
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("2.0", 0, &options, false, false);
        assert_eq!(v, "2.0");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_min_ver_bump_drops_source_ext() {
        // Source (1.7, 8), min (2.0, None) → min wins by ver → source_ext must
        // be dropped (pairwise rule: ext does not carry across a version bump).
        // Expected: (2.0, 0), NOT (2.0, 8).
        let options = WriterOptions {
            min_version: Some("2.0".into()),
            min_extension_level: None,
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
        assert_eq!(v, "2.0");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_min_ver_bump_replaces_source_ext_with_min_ext() {
        // Source (1.7, 8), min (2.0, Some(3)) → min wins by ver → source_ext
        // drops, min_ext carries → (2.0, 3).
        let options = WriterOptions {
            min_version: Some("2.0".into()),
            min_extension_level: Some(3),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
        assert_eq!(v, "2.0");
        assert_eq!(e, 3);
    }

    #[test]
    fn effective_pdf_version_and_ext_floor_bump_drops_both_ext() {
        // Source (1.3, 8), min unset, object_streams bumps to 1.5 → neither
        // source nor min contributes → ext = 0.
        let options = WriterOptions::default();
        let (v, e) = effective_pdf_version_and_ext("1.3", 8, &options, false, true);
        assert_eq!(v, "1.5");
        assert_eq!(e, 0);
    }

    #[test]
    fn encryption_version_floor_matches_qpdf_table() {
        use crate::encrypt_setup::{EncryptMethod, EncryptParams};
        for (method, expected) in [
            (EncryptMethod::V5R6Aes256, PdfVersion::new(1, 7, 8)),
            (EncryptMethod::V5R5Aes256, PdfVersion::new(1, 7, 3)),
            (EncryptMethod::V4Aes128, PdfVersion::new(1, 6, 0)),
            (EncryptMethod::V4Rc4128, PdfVersion::new(1, 5, 0)),
            (EncryptMethod::V2Rc4128, PdfVersion::new(1, 4, 0)),
            (EncryptMethod::V1Rc440, PdfVersion::new(1, 3, 0)),
        ] {
            let mut params = EncryptParams::v4_aes128(vec![], vec![]);
            params.method = method;
            let options = WriterOptions {
                encrypt: Some(params),
                ..WriterOptions::default()
            };
            assert_eq!(
                encryption_version_floor(&options),
                Some(expected),
                "encryption floor for {method:?}"
            );
        }
    }

    #[test]
    fn encryption_version_floor_copy_r5_uses_r5_extension_level() {
        use crate::encrypt_setup::CopyEncryptionSource;

        let mut encrypt_dict = Dictionary::new();
        encrypt_dict.insert("V", Object::Integer(5));
        encrypt_dict.insert("R", Object::Integer(5));
        let options = WriterOptions {
            copy_encryption: Some(CopyEncryptionSource {
                encrypt_dict,
                file_key: Vec::new(),
                id0: Vec::new(),
                object_key_alg: crate::ObjectKeyAlg::Aes,
            }),
            ..WriterOptions::default()
        };

        assert_eq!(
            encryption_version_floor(&options),
            Some(PdfVersion::new(1, 7, 3)),
            "a V=5/R=5 copied encryption source must not inherit the R=6 floor"
        );
    }

    #[test]
    fn effective_pdf_version_folds_each_encryption_floor_arm() {
        // Below-floor source for each encryption method — exercises every
        // PdfVersion::static_version_str arm reachable from the encryption floor race.
        use crate::encrypt_setup::{EncryptMethod, EncryptParams};
        for (method, expected) in [
            (EncryptMethod::V1Rc440, "1.3"),
            (EncryptMethod::V2Rc4128, "1.4"),
            (EncryptMethod::V4Rc4128, "1.5"),
            (EncryptMethod::V4Aes128, "1.6"),
            (EncryptMethod::V5R5Aes256, "1.7"),
            (EncryptMethod::V5R6Aes256, "1.7"),
        ] {
            let mut params = EncryptParams::v4_aes128(vec![], vec![]);
            params.method = method;
            let options = WriterOptions {
                encrypt: Some(params),
                ..WriterOptions::default()
            };
            // "1.0" is below every encryption floor, so the returned string
            // comes from the encryption-floor branch (via PdfVersion::static_version_str).
            assert_eq!(
                effective_pdf_version("1.0", &options, false, false),
                expected,
                "effective_pdf_version for {method:?}"
            );
        }
    }

    #[test]
    fn encryption_version_floor_none_when_no_encryption() {
        let options = WriterOptions::default();
        assert_eq!(encryption_version_floor(&options), None);
    }

    #[test]
    fn effective_pdf_version_folds_encryption_floor_into_header() {
        // Source PDF-1.3 + --encrypt AES-128 → header floor must be 1.6.
        // Before the fix, effective_pdf_version returned "1.3" and the emitted
        // %PDF header contradicted the encryption method.
        let options = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                vec![],
                vec![],
            )),
            ..WriterOptions::default()
        };
        assert_eq!(effective_pdf_version("1.3", &options, false, false), "1.6");
    }

    #[test]
    fn effective_pdf_version_and_ext_v5_r6_encryption_contributes_ext() {
        // Source (1.3, 0) + AES-256 encryption (V5R6, floor (1.7, 8)) → ver
        // bumps to 1.7, encryption contributes ext=8 (source's 1.3 was outbid,
        // no min_version, no ObjStm).
        let options = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v5_r6(vec![], vec![])),
            ..WriterOptions::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.3", 0, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 8);
    }

    #[test]
    fn effective_pdf_version_and_ext_aes128_encryption_drops_stale_source_ext() {
        // Codex round-3 P2 #3: source (1.3, 8) + AES-128 encryption (floor
        // (1.6, 0)) → ver bumps to 1.6, source ext dropped (source's 1.3
        // doesn't tie with 1.6). Result (1.6, 0) — stale ADBE must NOT survive.
        let options = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                vec![],
                vec![],
            )),
            ..WriterOptions::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.3", 8, &options, false, false);
        assert_eq!(v, "1.6");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_encryption_and_source_tie_take_max_ext() {
        // Source (1.7, 3) + AES-256 encryption (V5R6 floor (1.7, 8)) → both
        // tie at 1.7 → ext = max(3, 8) = 8 (encryption wins the extension race
        // at the tied version).
        let options = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v5_r6(vec![], vec![])),
            ..WriterOptions::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 3, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 8);
    }

    #[test]
    fn effective_pdf_version_and_ext_source_ext_wins_when_source_beats_encryption() {
        // Source (1.7, 8) + AES-128 encryption (floor (1.6, 0)) → source's 1.7
        // wins the version race, source ext survives → (1.7, 8). Encryption
        // floor was outbid so its ext=0 contributes nothing.
        let options = WriterOptions {
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                vec![],
                vec![],
            )),
            ..WriterOptions::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 8);
    }

    #[test]
    fn effective_pdf_version_and_ext_force_version_drops_source_ext() {
        // --force-version=1.7 on a (1.7, 8) source → qpdf semantics: forced
        // header is exact, extension level is 0 unless explicitly specified.
        // Currently there is no explicit force-extension knob, so the source
        // ext must not slip through just because the forced major/minor
        // happens to equal the source version.
        let options = WriterOptions {
            force_version: Some("1.7".into()),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_force_version_drops_min_ext() {
        // --force-version=1.7 + --min-version=1.7 --min-extension-level=8
        // → forced value is exact, so min_ext must not survive the tie.
        let options = WriterOptions {
            force_version: Some("1.7".into()),
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 0, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 0);
    }

    #[test]
    fn effective_pdf_version_and_ext_invalid_force_version_is_ignored() {
        // An unparseable --force-version is silently ignored by
        // effective_pdf_version (mirrors the existing behavior); the pairwise
        // helper must therefore fall back to normal contribution semantics
        // rather than treating any garbage-force as "forced=true".
        let options = WriterOptions {
            force_version: Some("not-a-version".into()),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 8);
    }

    #[test]
    fn effective_pdf_version_and_ext_source_ext_wins_when_versions_tie() {
        // Source (1.7, 8), min (1.7, 0) → tie on ver, source_ext higher → (1.7, 8).
        let options = WriterOptions {
            min_version: Some("1.7".into()),
            min_extension_level: Some(0),
            ..Default::default()
        };
        let (v, e) = effective_pdf_version_and_ext("1.7", 8, &options, false, false);
        assert_eq!(v, "1.7");
        assert_eq!(e, 8);
    }

    // ── inject_adbe_extension / min_extension_level end-to-end ──────────────
    //
    // These exercise the full-rewrite mutation that injects the Catalog's
    // /Extensions /ADBE dict when WriterOptions::min_extension_level requests
    // it. They use static_id for a stable output and inspect the emitted
    // bytes for the expected keys and header.

    #[test]
    fn inject_adbe_extension_reads_the_live_catalog_handle() {
        let mut pdf = crate::Pdf::open_mem(Arc::from(build_ext_injection_source()))
            .expect("fixture must open");
        let root = pdf.root_ref().expect("fixture must have a root");

        // Seed the legacy cache, then mutate only the canonical handle graph.
        // The writer consumer must not rebuild the Catalog from that stale
        // materialized snapshot.
        pdf.resolve(root).expect("catalog must resolve");
        let catalog = pdf.get_object_handle(root);
        pdf.resolve_object_handle(&catalog)
            .expect("canonical catalog must resolve");
        catalog
            .replace_key(
                b"/Extensions",
                ObjectHandle::dictionary(vec![(
                    b"/XYZW".to_vec(),
                    ObjectHandle::dictionary(vec![(b"/Value".to_vec(), ObjectHandle::integer(7))]),
                )]),
            )
            .expect("live Catalog mutation must succeed");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("canonical Catalog must belong to this PDF");
        let before = catalog
            .try_get_key(b"/Extensions")
            .expect("live Extensions lookup must succeed")
            .try_get_keys()
            .expect("live Extensions must be a dictionary");
        assert!(before.contains(b"/XYZW".as_slice()));

        inject_adbe_extension(&mut pdf, "1.7", 8).expect("injection must succeed");

        let catalog = pdf.get_object_handle(root);
        pdf.resolve_object_handle(&catalog)
            .expect("mutated Catalog must resolve");
        let extensions = catalog
            .try_get_key(b"/Extensions")
            .expect("Extensions lookup must succeed");
        let keys = extensions
            .try_get_keys()
            .expect("Extensions must remain a dictionary");
        assert!(keys.contains(b"/XYZW".as_slice()));
        assert!(keys.contains(b"/ADBE".as_slice()));
    }

    #[test]
    fn inject_adbe_extension_accepts_a_direct_catalog_stream() {
        let mut pdf = crate::Pdf::open_mem(Arc::from(build_ext_injection_source()))
            .expect("fixture must open");
        let root = pdf.root_ref().expect("fixture must have a root");
        let catalog = pdf.get_object_handle(root);
        pdf.resolve_object_handle(&catalog)
            .expect("canonical catalog must resolve");
        catalog
            .replace_key(
                b"/Metadata",
                ObjectHandle::stream(
                    ObjectHandle::dictionary(vec![(
                        b"/Type".to_vec(),
                        ObjectHandle::name(b"Metadata".to_vec()),
                    )]),
                    Rc::new(b"<xmp/>".to_vec()),
                ),
            )
            .expect("direct Catalog stream mutation must succeed");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("canonical Catalog must belong to this PDF");

        inject_adbe_extension(&mut pdf, "1.7", 8)
            .expect("a direct stream sibling must not block Catalog mutation");

        let catalog = pdf.get_object_handle(root);
        pdf.resolve_object_handle(&catalog)
            .expect("mutated Catalog must resolve");
        assert_eq!(
            catalog
                .try_get_key(b"/Metadata")
                .expect("Metadata lookup must succeed")
                .type_name(),
            "stream"
        );
    }

    #[test]
    fn writer_catalog_copy_requires_a_root() {
        let mut pdf = crate::Pdf::open_mem(Arc::from(
            b"%PDF-1.3\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\n\
              startxref\n9\n%%EOF\n" as &[u8],
        ))
        .expect("rootless fixture must open");

        let error = inject_adbe_extension(&mut pdf, "1.7", 8).expect_err("root is required");
        assert!(matches!(error, crate::Error::Missing("/Root")));
    }

    #[test]
    fn catalog_adbe_probe_ignores_a_non_dictionary_extensions_value() {
        let mut pdf = crate::Pdf::open_mem(Arc::from(build_ext_injection_source()))
            .expect("fixture must open");
        let root = pdf.root_ref().expect("fixture must have a root");
        let catalog = pdf.get_object_handle(root);
        pdf.resolve_object_handle(&catalog)
            .expect("canonical catalog must resolve");
        catalog
            .replace_key(b"/Extensions", ObjectHandle::integer(7))
            .expect("Catalog mutation must succeed");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("canonical Catalog must belong to this PDF");

        assert!(!catalog_has_extensions_adbe(&mut pdf).expect("probe must succeed"));
    }

    /// Build a minimal single-page-less classic 1.3 PDF with only a Catalog
    /// and empty Pages tree — the shape shared with the existing
    /// deterministic-id fixture builder.
    fn build_ext_injection_source() -> Vec<u8> {
        let mut src = b"%PDF-1.3\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        src
    }

    fn write_full_rewrite_with(source: &[u8], options: &WriterOptions) -> Vec<u8> {
        let mut pdf = crate::Pdf::open_mem(Arc::from(source)).expect("fixture must open");
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, options).expect("full-rewrite write must succeed");
        out
    }

    #[test]
    fn emit_canonical_pdf_injects_extensions_adbe_when_min_ext_gt_zero() {
        // Source is 1.3 with no /Extensions. min_version=1.7 wins the version
        // half; because eff_ver == options.min_version, the ext half is
        // max(source_ext=0, min_ext=8) = 8, so injection fires.
        let src = build_ext_injection_source();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);

        assert!(
            out.starts_with(b"%PDF-1.7\n"),
            "min_version=1.7 must raise the header; first 12 bytes: {:?}",
            &out[..out.len().min(12)] // cov:ignore: diagnostic format arg only evaluated on assertion failure
        );
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/Extensions"), "Catalog must carry /Extensions");
        assert!(s.contains("/ADBE"), "Extensions must carry /ADBE");
        assert!(
            s.contains("/BaseVersion /1.7"),
            "ADBE must carry /BaseVersion /1.7"
        );
        assert!(
            s.contains("/ExtensionLevel 8"),
            "ADBE must carry /ExtensionLevel 8"
        );
    }

    #[test]
    fn emit_canonical_pdf_no_injection_when_min_ext_is_none() {
        // Same version bump but no min_extension_level → header rises to 1.7
        // but no /Extensions injection, matching qpdf's --min-version=1.7.
        let src = build_ext_injection_source();
        let options = WriterOptions {
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: None,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);

        assert!(out.starts_with(b"%PDF-1.7\n"));
        let s = String::from_utf8_lossy(&out);
        assert!(
            !s.contains("/Extensions"),
            "no min_extension_level → no /Extensions injection"
        );
    }

    #[test]
    fn emit_canonical_pdf_no_injection_when_min_ext_is_zero() {
        // Effective extension level 0 must not trigger injection even when
        // min_extension_level is explicitly Some(0).
        let src = build_ext_injection_source();
        let options = WriterOptions {
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(0),
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);

        assert!(out.starts_with(b"%PDF-1.7\n"));
        let s = String::from_utf8_lossy(&out);
        assert!(
            !s.contains("/Extensions"),
            "eff_ext == 0 → no /Extensions injection"
        );
    }

    #[test]
    fn emit_canonical_pdf_ext_injection_preserves_non_adbe_developer_prefix() {
        // Source has a direct /Extensions dict with a non-ADBE developer
        // prefix. Injection must overwrite only /ADBE and leave the other
        // prefix intact — mirroring qpdf's QPDFWriter behaviour.
        let mut src = b"%PDF-1.3\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /Extensions << /XYZW << /BaseVersion /1.3 /ExtensionLevel 1 >> >> \
              >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);

        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/ADBE"), "injected /ADBE must appear");
        assert!(
            s.contains("/BaseVersion /1.7"),
            "new /ADBE must carry the effective /BaseVersion"
        );
        assert!(
            s.contains("/ExtensionLevel 8"),
            "new /ADBE must carry the effective /ExtensionLevel"
        );
        assert!(
            s.contains("/XYZW"),
            "non-ADBE developer prefix must survive: {s}"
        );
    }

    #[test]
    fn emit_canonical_pdf_ext_injection_follows_indirect_extensions_ref() {
        // Source /Extensions is an indirect reference to a dict that already
        // has an ADBE at an older level plus a non-ADBE prefix. Injection
        // must overwrite /ADBE with the new (BaseVersion, ExtensionLevel),
        // preserve the other prefix, and inline the result on the Catalog.
        let mut src = b"%PDF-1.3\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 3 0 R >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        offsets.push(src.len());
        src.extend_from_slice(
            b"3 0 obj\n<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> \
              /ACRO << /BaseVersion /1.7 /ExtensionLevel 1 >> >>\nendobj\n",
        );
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("/ExtensionLevel 8"),
            "/ADBE must have been overwritten with the new level: {s}"
        );
        assert!(
            !s.contains("/ExtensionLevel 3"),
            "old /ADBE /ExtensionLevel 3 must not survive: {s}"
        );
        assert!(
            s.contains("/ACRO"),
            "non-ADBE developer prefix must survive: {s}"
        );
    }

    /// PDF-1.3 source that already carries `/Extensions /ADBE /ExtensionLevel N`.
    /// Used by the ObjStm-floor regression tests below.
    fn build_ext_injection_source_with_adbe_1_3() -> Vec<u8> {
        let mut src = b"%PDF-1.3\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /Extensions << /ADBE << /BaseVersion /1.3 /ExtensionLevel 8 >> >> \
              >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        src
    }

    #[test]
    fn emit_canonical_pdf_generate_mode_strips_stale_adbe_on_floor_bump() {
        // Codex round-2 P2: source PDF-1.3 with /Extensions /ADBE /ExtensionLevel 8
        // rewritten with --object-streams=generate. The ObjStm floor bumps the
        // header to %PDF-1.5, so the source's ADBE (BaseVersion /1.3) must NOT
        // survive — otherwise the emitted Catalog contradicts the header.
        let src = build_ext_injection_source_with_adbe_1_3();
        // Sanity: baseline reader sees the source ext.
        {
            let mut src_pdf = crate::Pdf::open_mem(Arc::from(&src[..])).expect("source must open");
            assert_eq!(
                src_pdf.adobe_extension_level(),
                Some(8),
                "fixture must carry source ADBE ExtensionLevel 8"
            );
        }
        let options = WriterOptions {
            static_id: true,
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);
        assert!(
            out.starts_with(b"%PDF-1.5\n"),
            "ObjStm floor must bump the header to 1.5"
        );
        let mut reopened = crate::Pdf::open_mem_owned(out).expect("output must open");
        assert_eq!(
            reopened.adobe_extension_level(),
            None,
            "stale ADBE from source (BaseVersion 1.3) must not survive when the \
             ObjStm floor drops the effective ext to 0"
        );
    }

    #[test]
    fn emit_canonical_pdf_generate_mode_strips_stale_adbe_via_indirect_ref_preserving_other_prefix()
    {
        // Sibling of *_on_floor_bump but exercising the two remaining
        // strip_adbe_extension branches: (1) /Extensions is an *indirect*
        // reference (line 711), and (2) the source /Extensions carries a
        // non-ADBE developer prefix that must survive after /ADBE is
        // removed (line 723). Same trigger: ObjStm floor bumps 1.3 → 1.5.
        let mut src = b"%PDF-1.3\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 3 0 R >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        offsets.push(src.len());
        src.extend_from_slice(
            b"3 0 obj\n<< /ADBE << /BaseVersion /1.3 /ExtensionLevel 8 >> \
              /XYZW << /BaseVersion /1.3 /ExtensionLevel 1 >> >>\nendobj\n",
        );
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        {
            let mut src_pdf = crate::Pdf::open_mem(Arc::from(&src[..])).expect("source must open");
            assert_eq!(
                src_pdf.adobe_extension_level(),
                Some(8),
                "fixture must carry source ADBE ExtensionLevel 8 through an indirect ref"
            );
        }
        let options = WriterOptions {
            static_id: true,
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);
        assert!(
            out.starts_with(b"%PDF-1.5\n"),
            "ObjStm floor must bump the header to 1.5"
        );
        let mut reopened = crate::Pdf::open_mem_owned(out).expect("output must open");
        assert_eq!(
            reopened.adobe_extension_level(),
            None,
            "stale ADBE from source must be stripped when the ObjStm floor \
             drops the effective ext to 0"
        );
        // The non-ADBE developer prefix must survive the strip. Walk the
        // Catalog manually. strip_adbe_extension inlines the mutated
        // /Extensions back as a direct Dictionary on the Catalog, so we can
        // access it directly without an extra resolve step. Extract owned
        // Catalog dict via resolve+into_dict so the mutable borrow on
        // `reopened` used by resolve() is released before subsequent asserts.
        let root_ref = reopened.trailer().get_ref("Root").expect("Root ref");
        let catalog_dict = reopened
            .resolve(root_ref)
            .expect("resolve root")
            .into_dict()
            .expect("root is dict");
        // strip_adbe_extension always inlines /Extensions as a direct
        // Dictionary, so a non-Dict variant here would indicate a code bug —
        // panic rather than silently pass. Using as_dict keeps the assertion
        // failure informative if this invariant ever breaks.
        let ext_dict = catalog_dict
            .get("Extensions")
            .expect("Extensions key survives strip")
            .as_dict()
            .expect("Extensions must be present as a direct dict after strip");
        assert!(
            ext_dict.get("ADBE").is_none(),
            "ADBE must be gone: {ext_dict:?}"
        );
        assert!(
            ext_dict.get("XYZW").is_some(),
            "non-ADBE developer prefix must survive: {ext_dict:?}"
        );
    }

    #[test]
    fn emit_canonical_pdf_generate_mode_still_injects_extensions_adbe() {
        // The output-only ADBE mutation must happen before the shared plain plan
        // snapshots and renumbers the Catalog. Generate packs the Catalog into a
        // FlateDecoded object stream, so verify structurally after reopening.
        let src = build_ext_injection_source();
        let options = WriterOptions {
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);

        assert!(out.starts_with(b"%PDF-1.7\n"), "header must raise to 1.7");
        let mut reopened = crate::Pdf::open_mem_owned(out).expect("output must open");
        assert_eq!(
            reopened.adobe_extension_level(),
            Some(8),
            "generate-mode output must carry /Extensions /ADBE /ExtensionLevel 8"
        );
    }

    #[test]
    fn emit_canonical_pdf_does_not_dirty_caller_pdf_across_writes() {
        // Codex round-3 P2 #4: the injection block mutates the caller's Pdf
        // via set_object. Without the save/restore in emit_canonical_pdf,
        // reusing the same Pdf handle for a second write would leak the
        // output-only /Extensions dict into the second output. Verify the
        // caller sees the same Pdf state before and after a write, and that
        // two back-to-back writes produce identical output.
        let src = build_ext_injection_source();
        let mut pdf = crate::Pdf::open_mem_owned(src).expect("open");
        // Pre-write baseline: source has no ADBE.
        assert_eq!(pdf.adobe_extension_level(), None);

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..WriterOptions::default()
        };
        let mut out1 = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out1, &options).expect("first write");

        // Caller-visible Pdf state must be unchanged after the write —
        // adobe_extension_level still None (mutation was restored).
        assert_eq!(
            pdf.adobe_extension_level(),
            None,
            "first write must not leak /Extensions into the caller's Pdf"
        );

        // A second write with the same options must produce byte-identical
        // output. If the first write had dirtied the Pdf, the second would
        // see a different source_ext and could inject differently or accrete
        // duplicate /Extensions state.
        let mut out2 = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out2, &options).expect("second write");
        assert_eq!(
            out1, out2,
            "back-to-back writes on the same Pdf must be byte-identical"
        );

        // And the output must still carry the requested /ADBE injection.
        let mut reopened = crate::Pdf::open_mem_owned(out1).expect("reopen");
        assert_eq!(
            reopened.adobe_extension_level(),
            Some(8),
            "output must carry the requested ADBE"
        );
    }

    #[test]
    fn emit_canonical_pdf_does_not_leave_root_dirty_flag_set() {
        // Codex round-4 P2 #1: even after restoring the pre-write Catalog,
        // `Pdf::set_object` used for the restore unconditionally marks the
        // ref dirty. A subsequent incremental `write_pdf` would then emit a
        // no-op Catalog delta, breaking the signed-prefix-preserving no-op
        // workflow. Verify the outer wrapper clears the dirty flag when the
        // ref was clean before the write.
        let src = build_ext_injection_source();
        let mut pdf = crate::Pdf::open_mem_owned(src).expect("open");
        let root_ref = pdf.root_ref().expect("Root");
        // Baseline: Root must be clean before any write.
        assert!(!pdf.is_dirty(root_ref), "Root must be clean pre-write");

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("write");

        // Root must remain clean after the write — the injection's mutation
        // was fully rolled back (value + dirty flag).
        assert!(
            !pdf.is_dirty(root_ref),
            "Root dirty flag must be cleared after the full-rewrite restore"
        );
    }

    #[test]
    fn emit_canonical_pdf_preserves_pre_existing_root_dirty_flag() {
        // Sibling of _does_not_leave_root_dirty_flag_set: if the caller had
        // already marked Root dirty BEFORE the full-rewrite (e.g. via a prior
        // set_object), the restore path must LEAVE the dirty flag set —
        // clearing it would silently drop the caller's own mutation.
        let src = build_ext_injection_source();
        let mut pdf = crate::Pdf::open_mem_owned(src).expect("open");
        let root_ref = pdf.root_ref().expect("Root");

        // Caller-side mutation: mark Root dirty explicitly by re-storing its
        // current value. set_object always dirties the ref regardless of
        // whether the value actually changed.
        let catalog = pdf.resolve(root_ref).expect("resolve root");
        pdf.set_object(root_ref, catalog);
        assert!(pdf.is_dirty(root_ref), "sanity: caller marked Root dirty");

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            min_version: Some("1.7".into()),
            min_extension_level: Some(8),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("write");

        assert!(
            pdf.is_dirty(root_ref),
            "pre-existing Root dirty flag must survive the full-rewrite restore"
        );
    }

    #[test]
    fn emit_canonical_pdf_v4_aes128_source_1_3_with_adbe_strips_stale_ext() {
        // Codex round-3 P2 #3: source PDF-1.3 with /Extensions /ADBE
        // /ExtensionLevel 8, rewritten with --encrypt AES-128. The encryption
        // floor bumps the header to 1.6; the source's ADBE (BaseVersion 1.3
        // ext 8) must NOT survive because the pairwise rule drops the source
        // ext when the version is outbid.
        let src = build_ext_injection_source_with_adbe_1_3();
        let mut pdf = crate::Pdf::open_mem_owned(src).expect("open");
        assert_eq!(pdf.adobe_extension_level(), Some(8));

        let options = WriterOptions {
            static_id: true,
            static_aes_iv: true,
            encrypt: Some(crate::encrypt_setup::EncryptParams::v4_aes128(
                b"u".to_vec(),
                b"o".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("aes-128 encrypted write");

        // cov:ignore-start: multi-line assert; llvm-cov attributes only the
        // "on-panic" format-argument evaluations to the outer line.
        assert!(
            out.starts_with(b"%PDF-1.6\n"),
            "AES-128 must floor the header to 1.6, got {:?}",
            String::from_utf8_lossy(&out[..out.len().min(12)])
        );
        // cov:ignore-end

        // Re-open the encrypted output with the same password and confirm
        // there is no residual /ADBE with a stale /BaseVersion.
        let reopen_opts = crate::PdfOpenOptions {
            password: b"u".to_vec(),
            ..Default::default()
        };
        let mut reopened =
            crate::Pdf::open_mem_owned_with_options(out, reopen_opts).expect("reopen encrypted");
        assert_eq!(
            reopened.adobe_extension_level(),
            None,
            "stale /ADBE (BaseVersion 1.3) must be stripped by the encryption floor"
        );
    }

    #[test]
    fn emit_canonical_pdf_strips_stale_adbe_when_source_has_no_extension_level() {
        // qpdf QPDFWriter.cc L1387/L1408 (whole /Extensions removed) parity:
        // source /Extensions /ADBE dict has no /ExtensionLevel (or non-integer).
        // adobe_extension_level() returns None → source_ext = 0. The pre-broadening
        // trigger (`source_ext > 0`) would skip strip and let /ADBE pass through;
        // the broadened trigger (`catalog_has_extensions_adbe`) fires and drops
        // /Extensions entirely because /ADBE is the only key.
        let mut src = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /Extensions << /ADBE << /BaseVersion /1.4 >> >> >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        // Sanity: adobe_extension_level() returns None because /ExtensionLevel is absent.
        {
            let mut src_pdf = crate::Pdf::open_mem(Arc::from(&src[..])).expect("source must open");
            assert_eq!(src_pdf.adobe_extension_level(), None);
        }
        let options = WriterOptions {
            static_id: true,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);
        let mut reopened = crate::Pdf::open_mem(Arc::from(&out[..])).expect("output must open");
        // The whole /Extensions dict must be gone from the output Catalog.
        let root_ref = reopened.trailer().get_ref("Root").expect("Root ref");
        let catalog = reopened
            .resolve(root_ref)
            .expect("resolve root")
            .into_dict()
            .expect("root is dict");
        assert!(
            catalog.get("Extensions").is_none(),
            "stale /ADBE without /ExtensionLevel must trigger whole-/Extensions removal: {catalog:?}"
        );
    }

    #[test]
    fn emit_canonical_pdf_strips_stale_adbe_no_ext_level_preserving_vendor_prefix() {
        // qpdf QPDFWriter.cc L1432 (removeKey /ADBE, keep other extensions) parity:
        // source /Extensions has /ADBE without /ExtensionLevel AND a non-ADBE
        // developer prefix (/XYZW). Broadened trigger must fire and strip /ADBE
        // only, leaving /XYZW intact.
        let mut src = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /Extensions << /ADBE << /BaseVersion /1.4 >> \
              /XYZW << /BaseVersion /1.4 /ExtensionLevel 1 >> >> >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        // Sanity: adobe_extension_level() returns None (source /ExtensionLevel absent).
        {
            let mut src_pdf = crate::Pdf::open_mem(Arc::from(&src[..])).expect("source must open");
            assert_eq!(src_pdf.adobe_extension_level(), None);
        }
        let options = WriterOptions {
            static_id: true,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);
        let mut reopened = crate::Pdf::open_mem(Arc::from(&out[..])).expect("output must open");
        let root_ref = reopened.trailer().get_ref("Root").expect("Root ref");
        let catalog = reopened
            .resolve(root_ref)
            .expect("resolve root")
            .into_dict()
            .expect("root is dict");
        // /Extensions must still be present, containing only /XYZW.
        let ext_dict = catalog
            .get("Extensions")
            .expect("/Extensions must survive because /XYZW is present")
            .as_dict()
            .expect("/Extensions must be a direct dict after strip");
        assert!(
            ext_dict.get("ADBE").is_none(),
            "stale /ADBE without /ExtensionLevel must be stripped: {ext_dict:?}"
        );
        assert!(
            ext_dict.get("XYZW").is_some(),
            "non-ADBE developer prefix must survive: {ext_dict:?}"
        );
    }

    #[test]
    fn emit_canonical_pdf_strips_stale_adbe_via_indirect_extensions_no_ext_level() {
        // qpdf QPDFWriter.cc L1387 parity: `/Extensions` may arrive as an indirect
        // reference. The broadened trigger must resolve the ref before checking
        // for /ADBE key existence, and strip must inline the resolved dict then
        // remove /ADBE. Covers the `Object::Reference` arm of
        // `catalog_has_extensions_adbe` that inline-`/Extensions` fixtures skip.
        let mut src = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        offsets.push(src.len());
        src.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 3 0 R >>\nendobj\n",
        );
        offsets.push(src.len());
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        offsets.push(src.len());
        src.extend_from_slice(
            b"3 0 obj\n<< /ADBE << /BaseVersion /1.4 >> \
              /XYZW << /BaseVersion /1.4 /ExtensionLevel 1 >> >>\nendobj\n",
        );
        let startxref = src.len();
        let count = offsets.len() + 1;
        src.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            src.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        src.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R >>\n\
                 startxref\n{startxref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        // Sanity: adobe_extension_level returns None (source /ExtensionLevel absent
        // even after resolving the indirect /Extensions ref).
        {
            let mut src_pdf = crate::Pdf::open_mem(Arc::from(&src[..])).expect("source must open");
            assert_eq!(src_pdf.adobe_extension_level(), None);
        }
        let options = WriterOptions {
            static_id: true,
            ..WriterOptions::default()
        };
        let out = write_full_rewrite_with(&src, &options);
        let mut reopened = crate::Pdf::open_mem(Arc::from(&out[..])).expect("output must open");
        let root_ref = reopened.trailer().get_ref("Root").expect("Root ref");
        let catalog = reopened
            .resolve(root_ref)
            .expect("resolve root")
            .into_dict()
            .expect("root is dict");
        // strip inlines /Extensions as a direct dict; /ADBE gone, /XYZW survives.
        let ext_dict = catalog
            .get("Extensions")
            .expect("/Extensions must survive because /XYZW is present")
            .as_dict()
            .expect("/Extensions must be inlined as a direct dict after strip");
        assert!(
            ext_dict.get("ADBE").is_none(),
            "stale /ADBE from indirect /Extensions must be stripped: {ext_dict:?}"
        );
        assert!(
            ext_dict.get("XYZW").is_some(),
            "non-ADBE developer prefix from indirect /Extensions must survive: {ext_dict:?}"
        );
    }

    #[test]
    fn source_encrypted_generate_keeps_legacy_container_numbering() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
        let file = std::fs::File::open(fixture).expect("open encrypted fixture");
        let mut pdf = crate::Pdf::open(std::io::BufReader::new(file))
            .expect("fixture authenticates with the default empty password");
        assert!(
            pdf.encryption_ref().is_some(),
            "route sentinel requires a source-encrypted PDF"
        );

        let options = WriterOptions {
            static_id: true,
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let mut out = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut out, &options).expect("Generate rewrite");

        let reopened = crate::Pdf::open_mem_owned(out).expect("reopen unencrypted output");
        let compressed: Vec<(ObjectRef, u32)> = reopened
            .source_xref_entries()
            .into_iter()
            .filter_map(|(member, offset)| match offset {
                XrefEntry::Compressed { stream, .. } => Some((member, stream)),
                _ => None,
            })
            .collect();
        assert!(
            !compressed.is_empty(),
            "Generate must still emit an object stream on source-encrypted input"
        );
        assert!(
            compressed
                .iter()
                .all(|(member, container)| *container > member.number),
            "source encryption must keep the legacy containers-above-members route: {compressed:?}"
        );
    }

    #[test]
    fn progress_reporter_debug_uses_qpdf_writer_shape() {
        let reporter = ProgressReporter::new(Box::new(|_| {}));

        assert_eq!(format!("{reporter:?}"), "ProgressReporter(..)");
    }

    #[test]
    fn configure_progress_reports_from_the_scaled_event_budget() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let events_for_reporter = Rc::clone(&events);
        let options = WriterOptions {
            progress_reporter: Some(ProgressReporter::new(Box::new(move |percent| {
                events_for_reporter.borrow_mut().push(percent);
            }))),
            ..WriterOptions::default()
        };

        configure_progress(&options, 2, true);
        report_progress_event(&options);
        report_progress_finished(&options);

        assert_eq!(&*events.borrow(), &[0, 100]);
    }

    #[test]
    fn xref_stream_bytes_reject_overflow_and_missing_entries() {
        let overflow = build_xref_stream_bytes(&BTreeMap::new(), &[(u32::MAX, 1)])
            .expect_err("a range ending past u32::MAX must be rejected");
        assert!(matches!(overflow, crate::Error::Unsupported(message)
            if message == "xref stream range does not fit u32"));

        let offsets = BTreeMap::from([(1_u32, (0_u16, XrefEntry::Uncompressed { offset: 0 }))]);
        let missing = build_xref_stream_bytes(&offsets, &[(1, 2)])
            .expect_err("a range with a missing object entry must be rejected");
        assert!(matches!(missing, crate::Error::Unsupported(message)
            if message == "xref stream is missing object entry"));
    }

    fn copy_encryption_dictionary(version: i64, revision: i64, length: i64) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("V", Object::Integer(version));
        dict.insert("R", Object::Integer(revision));
        dict.insert("Length", Object::Integer(length));
        dict
    }

    fn copy_encryption_source(
        dict: Dictionary,
        file_key_len: usize,
    ) -> crate::CopyEncryptionSource {
        crate::CopyEncryptionSource {
            encrypt_dict: dict,
            file_key: vec![0; file_key_len],
            id0: vec![0; 16],
            object_key_alg: crate::ObjectKeyAlg::Rc4,
        }
    }

    #[test]
    fn canonical_copy_encryption_rejects_invalid_standard_handler_shapes() {
        let mut version_out_of_range = copy_encryption_dictionary(i64::from(i32::MAX) + 1, 2, 40);
        version_out_of_range.insert("P", Object::Integer(-4));
        let error = canonical_copy_encryption(&copy_encryption_source(version_out_of_range, 5))
            .expect_err("an out-of-range /V must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("/V is out of range")));

        let revision_out_of_range = copy_encryption_dictionary(1, i64::from(i32::MAX) + 1, 40);
        let error = canonical_copy_encryption(&copy_encryption_source(revision_out_of_range, 5))
            .expect_err("an out-of-range /R must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("/R is out of range")));

        let invalid_length = copy_encryption_dictionary(2, 3, 41);
        let error = canonical_copy_encryption(&copy_encryption_source(invalid_length, 16))
            .expect_err("a non-byte-aligned /Length must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("/Length is invalid")));

        let unsupported_v5 = copy_encryption_dictionary(5, 4, 256);
        let error = canonical_copy_encryption(&copy_encryption_source(unsupported_v5, 32))
            .expect_err("an unsupported V=5 revision must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("unsupported copy-encryption Standard handler V=5")));

        let unsupported_legacy = copy_encryption_dictionary(3, 3, 128);
        let error = canonical_copy_encryption(&copy_encryption_source(unsupported_legacy, 16))
            .expect_err("an unsupported legacy handler must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message.contains("unsupported copy-encryption Standard handler V=3")));
    }

    #[test]
    fn canonical_copy_encryption_rejects_missing_required_values() {
        let missing_version = Dictionary::new();
        let error = canonical_copy_encryption(&copy_encryption_source(missing_version, 5))
            .expect_err("missing /V must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message == "copy-encryption /V must be an integer"));

        let mut missing_owner = copy_encryption_dictionary(1, 2, 40);
        missing_owner.insert("P", Object::Integer(-4));
        let error = canonical_copy_encryption(&copy_encryption_source(missing_owner, 5))
            .expect_err("missing /O must be rejected");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message == "copy-encryption /O must be a string"));
    }

    #[test]
    fn filter_chain_classification_covers_qpdf_aliases_and_malformed_values() {
        let malformed = Object::Integer(1);
        assert!(!filter_chain_is_decodable(
            Some(&malformed),
            CompressStreams::Yes,
            DecodeLevel::All,
            false
        ));

        let malformed_item = Object::Array(vec![Object::Integer(1)]);
        assert!(!filter_chain_is_decodable(
            Some(&malformed_item),
            CompressStreams::Yes,
            DecodeLevel::All,
            false
        ));

        for alias in [
            b"Fl".as_slice(),
            b"LZW".as_slice(),
            b"A85".as_slice(),
            b"AHx".as_slice(),
        ] {
            let filter = Object::Name(alias.to_vec());
            assert!(filter_chain_is_decodable(
                Some(&filter),
                CompressStreams::Yes,
                DecodeLevel::None,
                false
            ));
        }

        let run_length = Object::Name(b"RL".to_vec());
        assert!(filter_chain_is_decodable(
            Some(&run_length),
            CompressStreams::No,
            DecodeLevel::Specialized,
            false
        ));

        let unknown = Object::Name(b"UnknownDecode".to_vec());
        assert!(!filter_chain_is_decodable(
            Some(&unknown),
            CompressStreams::Yes,
            DecodeLevel::All,
            false
        ));
    }

    #[test]
    fn content_helpers_ignore_non_container_values() {
        let mut pdf = crate::Pdf::open_mem_owned(build_partition_fixture()).expect("open fixture");
        pdf.set_object(ObjectRef::new(3, 0), Object::Integer(42));
        let mut containers = BTreeSet::new();
        collect_content_container_refs(&mut pdf, ObjectRef::new(3, 0), &mut containers)
            .expect("a non-dictionary page value is ignored");
        assert!(containers.is_empty());
    }

    #[test]
    fn pclm_writes_extra_header_and_deterministic_id() {
        let mut pdf = crate::Pdf::open_mem_owned(build_partition_fixture()).expect("open fixture");
        let options = WriterOptions {
            deterministic_id: true,
            extra_header_text: "% PCLm extra".to_string(),
            pclm: true,
            ..WriterOptions::default()
        };
        let mut output = Vec::new();
        emit_canonical_pdf(&mut pdf, &mut output, &options).expect("PCLm rewrite");

        assert!(output.starts_with(b"%PDF-1.4\n%PCLm 1.0\n% PCLm extra\n"));
        assert!(output
            .windows(b"/ID [<".len())
            .any(|window| window == b"/ID [<"));
    }

    #[test]
    fn pclm_rejects_conflicting_id_modes() {
        let mut pdf = crate::Pdf::open_mem_owned(build_partition_fixture()).expect("open fixture");
        let options = WriterOptions {
            deterministic_id: true,
            static_id: true,
            pclm: true,
            ..WriterOptions::default()
        };
        let error = emit_canonical_pdf(&mut pdf, Vec::new(), &options)
            .expect_err("PCLm must reject deterministic-id plus static-id");
        assert!(matches!(error, crate::Error::Unsupported(message)
            if message == "deterministic_id and static_id are mutually exclusive"));
    }
}
