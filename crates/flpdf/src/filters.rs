//! qpdf correspondence: QPDF_Stream filter-chain orchestration; QPDFStreamFilter dispatch, codec construction, and Pipeline execution are delegated to stream_filter.
use std::borrow::Cow;

use crate::ascii_hex;
use crate::object_handle::ObjectHandle;
#[cfg(test)]
use crate::pipeline::test_support::ascii85_fixture_bytes;
use crate::pipeline::{PipelineError, PipelineResult};
#[cfg(test)]
use crate::stream_filter::expect_first_filter_input;
use crate::stream_filter::{
    decode_filter_specs_from_handle, decode_filter_specs_from_object, encode_flate,
    encode_run_length, is_decoded_filter as stream_is_decoded_filter,
    passthrough_codec_label as stream_passthrough_codec_label, stream_filter_for,
    undecodable_filter_error, validate_filter_chain_count, DecodeParams, FilterDecodePhase,
    FilterSpec, CRYPT_STAGE_UNSUPPORTED, DECODE_OUTPUT_LIMIT_PREFIX,
};
use crate::{Dictionary, Error, Object, Result};

/// Maximum number of stages a `/Filter` chain may declare on the **decode**
/// path. Real PDFs use at most a few stages; this rejects only pathological
/// input where each stage re-expands the previous (multiplicative blow-up).
/// Unlike qpdf — which imposes no chain-length cap — flpdf rejects such chains
/// outright; this is an intentional divergence, not a compatibility target.
/// The encode path (writer output, not untrusted) is not capped.
const MAX_FILTER_CHAIN_LEN: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamFilterCapabilities {
    pub(crate) specialized_compression: bool,
    pub(crate) lossy_compression: bool,
}

/// Return qpdf-compatible filterability and compression classification for a
/// registered flpdf stream-filter chain.
///
/// QPDF_Stream::filterable (libqpdf/QPDF_Stream.cc:386-482) validates the
/// filter chain and its /DecodeParms, then reports specialized and lossy
/// compression to pipeStreamData (libqpdf/QPDF_Stream.cc:504-512). The JSON
/// writer needs that classification before deciding whether a successful
/// decode may remove /Filter and /DecodeParms.
///
/// None means the stream is not filterable through flpdf's registered
/// filters. The caller preserves the raw payload, matching qpdf for an
/// unsupported filter and for compression outside the requested decode level.
pub(crate) fn stream_filter_capabilities(dict: &Dictionary) -> Option<StreamFilterCapabilities> {
    let specs = decode_filter_specs_from_object(
        dict.get("Filter"),
        dict.get("DecodeParms"),
        Some(MAX_FILTER_CHAIN_LEN),
    )
    .ok()?;

    let mut capabilities = StreamFilterCapabilities::default();
    for spec in specs {
        let mut filter = stream_filter_for(spec.normalized_name())?;
        if !filter.set_decode_params(&spec.decode_params) {
            return None;
        }
        capabilities.specialized_compression |= filter.is_specialized_compression();
        capabilities.lossy_compression |= filter.is_lossy_compression();
    }
    Some(capabilities)
}

pub(crate) fn validate_filter_chain_len(filters: &[Object]) -> Result<()> {
    validate_filter_chain_count(filters.len(), Some(MAX_FILTER_CHAIN_LEN))
}

/// Return a human-readable codec label if `filter_name` is one of the four
/// image/binary codecs (`DCTDecode`, `JBIG2Decode`, `JPXDecode`,
/// `CCITTFaxDecode`) that the writer always emits verbatim rather than
/// re-encoding.
///
/// This is an **encode-side** classification: it does not indicate whether
/// [`decode_stream_data`] can decode the codec. `DCTDecode` streams, for
/// example, are still reported here (the writer never re-encodes JPEG data)
/// even though `decode_stream_data` decodes them. Callers that need to know
/// whether a filter is decodable should use [`is_decoded_filter`] instead.
///
/// Comparison is **byte-exact** (PDF names are case-sensitive per spec).
/// Returns `None` for any other filter name.
pub fn passthrough_codec_label(filter_name: &[u8]) -> Option<&'static str> {
    stream_passthrough_codec_label(filter_name)
}

/// Return whether [`decode_stream_data`] can decode a single-stage `/Filter`
/// of `filter_name`.
///
/// Comparison is **byte-exact** (PDF names are case-sensitive per spec) and
/// this function performs no filter-name normalization, so a qpdf
/// abbreviation such as `DCT` returns `false` even though the expanded name
/// `DCTDecode` returns `true`. [`decode_stream_data`] normalizes internally
/// and decodes either spelling; this function is for callers that need to
/// know decodability in advance without decoding.
pub fn is_decoded_filter(filter_name: &[u8]) -> bool {
    stream_is_decoded_filter(filter_name)
}

/// Decode `stream_data` by applying the stream dictionary's `/Filter` chain,
/// honoring any `/DecodeParms`.
///
/// PNG predictors (`/Predictor 10` through `/Predictor 15`) and the TIFF
/// predictor (`/Predictor 2`) are applied as part of the chain.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when:
/// - a `/Filter` entry is an unknown or unimplemented codec, or a `Crypt`
///   filter (decryption is not performed by this entry point).
/// - `/Filter` is neither a name nor an array of names.
/// - a `/Filter` array declares more than 16 stages (the decode-path chain-length
///   cap, which rejects pathological multiplicative-expansion chains).
/// - `/DecodeParms` selects a `/Predictor` outside `1`, `2`, and `10..=15`, or
///   gives a non-integer value for a parameter the filter reads.
/// - a predictor's row geometry is invalid: a negative `/Columns`, `/Colors`, or
///   `/BitsPerComponent`, a PNG `/BitsPerComponent` outside `1`, `2`, `4`, `8`,
///   and `16`, or a row width that is zero. TIFF predictor bit widths follow
///   qpdf's `Pl_TIFFPredictor` constructor accepts widths through `64`; its
///   `BitStream`/`BitWriter` processing limit is `32` bits, as in qpdf.
/// - an implemented codec fails on malformed input — corrupt deflate, LZW,
///   ASCII85, ASCIIHex, or RunLength data.
pub fn decode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    decode_stream_data_with_limits_and_warnings(
        dict,
        stream_data,
        DecodeLimits::default(),
        &mut reject_decode_warning,
    )
}

/// A non-fatal warning emitted while decoding a stream codec.
///
/// The message and numeric code correspond to qpdf's `Pl_Flate` warning
/// callback. In particular, truncated Flate input reports zlib code `-5`
/// without turning a successfully built filter chain into an outer error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamDecodeWarning {
    /// qpdf-compatible warning text without document/object location context.
    pub message: String,
    /// Codec-specific numeric error code (a zlib code for Flate warnings).
    pub code: i32,
}

/// One ordered output or diagnostic emitted by an opt-in recoverable stream decode.
///
/// Events retain pipeline emission order. In particular, an error raised by
/// an outer filter's `write` precedes warnings emitted while its downstream
/// pipeline is subsequently finished.
#[derive(Debug)]
pub enum StreamDecodeEvent {
    /// A recovered decoded output chunk.
    Data(Vec<u8>),
    /// A non-fatal codec warning.
    Warning(StreamDecodeWarning),
    /// The first runtime codec failure after successful pipeline construction.
    Error(Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataEventMode {
    Record,
    Suppress,
}

impl DataEventMode {
    fn push(self, events: &mut Vec<StreamDecodeEvent>, data: &[u8]) {
        if self == Self::Record && !data.is_empty() {
            events.push(StreamDecodeEvent::Data(data.to_vec()));
        }
    }
}

/// Data and ordered diagnostics produced by an opt-in recoverable stream decode.
///
/// An outer [`Result::Err`] from [`decode_stream_data_recovering`] means the
/// filter chain could not be interpreted or constructed. Once construction
/// succeeds, a codec failure is stored as a [`StreamDecodeEvent::Error`], and
/// [`data`](Self::data) retains bytes already emitted before that failure.
#[derive(Debug)]
pub struct StreamDecodeOutcome {
    /// Bytes emitted by the constructed decode pipeline.
    pub data: Vec<u8>,
    /// Recovered output and diagnostics in pipeline emission order.
    pub events: Vec<StreamDecodeEvent>,
}

/// Decode a stream while preserving output emitted before a codec failure.
///
/// Unlike [`decode_stream_data`], this opt-in boundary separates filter-chain
/// interpretation/construction from runtime codec failure. Unsupported filter
/// shapes, names, and decode parameters return an outer [`Error`]. A runtime
/// error after successful construction returns [`StreamDecodeOutcome`] with
/// its partial bytes and error populated. This applies
/// [`DecodeLimits::default()`], including the default 16-stage `/Filter` cap.
pub fn decode_stream_data_recovering(
    dict: &Dictionary,
    stream_data: &[u8],
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_recovering_with_limits(dict, stream_data, DecodeLimits::default())
}

/// Decode a stream with explicit limits while retaining ordered recovery events.
///
/// # Errors
///
/// Returns an outer [`Error`] when the filter chain cannot be interpreted or
/// constructed, including when it exceeds [`DecodeLimits::max_filter_chain`].
/// Runtime codec failures instead remain ordered [`StreamDecodeEvent::Error`]
/// events alongside any recovered output, as for [`decode_stream_data_recovering`].
pub fn decode_stream_data_recovering_with_limits(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_recovering_with_limits_and_mode(
        dict,
        stream_data,
        limits,
        DataEventMode::Record,
    )
}

/// Opt-in limits applied while decoding a stream's filter chain.
///
/// By default, output is unlimited, matching [`decode_stream_data`], while the
/// `/Filter` chain is capped at 16 stages. Embedders processing untrusted input
/// can set [`max_output`](Self::max_output) to bound the decoded size of each
/// `FlateDecode`, `LZWDecode`, `ASCII85Decode`, `ASCIIHexDecode`, or
/// `RunLengthDecode` stage, trading completeness for a per-stage bound. It is
/// not a ceiling on the total work or cumulative output across a filter chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum decoded byte count permitted out of any single supported filter
    /// stage, counted after that stage's predictor if it has one. `None`
    /// (default) is unlimited.
    pub max_output: Option<usize>,
    /// Maximum `/Filter` stages accepted before individual filter items are
    /// validated. `None` disables this count limit.
    pub max_filter_chain: Option<usize>,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_output: None,
            max_filter_chain: Some(MAX_FILTER_CHAIN_LEN),
        }
    }
}

fn reject_decode_warning(message: &str, code: i32) -> PipelineResult<()> {
    Err(PipelineError::runtime(format!(
        "stream inflate: {message} (zlib error {code})"
    )))
}

pub(crate) fn decode_stream_data_with_limits_and_warnings(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    let outcome = decode_stream_data_recovering_with_limits_and_mode(
        dict,
        stream_data,
        limits,
        DataEventMode::Suppress,
    )?;
    replay_strict_decode_outcome(outcome, warn)
}

/// Collapse a recovered outcome into the strict `getStreamData` shape: the
/// first replayed error wins, otherwise the decoded bytes.
///
/// Shared by the legacy [`decode_stream_data_with_limits`] path and the
/// `ObjectHandle`-native [`decode_stream_data_from_handle`], so "which event
/// becomes the error" has one definition.
fn replay_strict_decode_outcome(
    outcome: StreamDecodeOutcome,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    let mut first_error = None;
    for event in outcome.events {
        let event_error = replay_strict_decode_event(event, warn);
        if first_error.is_none() {
            first_error = event_error;
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(outcome.data),
    }
}

fn replay_strict_decode_event(
    event: StreamDecodeEvent,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Option<Error> {
    match event {
        StreamDecodeEvent::Data(_) => None,
        StreamDecodeEvent::Warning(warning) => warn(&warning.message, warning.code)
            .err()
            .map(|error| Error::Unsupported(error.into_string_lossy())),
        StreamDecodeEvent::Error(error) => Some(error),
    }
}

fn decode_stream_data_recovering_with_limits_and_mode(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_with_filters(
        dict.get("Filter"),
        dict.get("DecodeParms"),
        stream_data,
        limits,
        data_events,
    )
}

/// Returns `true` when `error` is the limit-exceeded signal raised when a
/// supported filter stage aborts because its output would exceed
/// [`DecodeLimits::max_output`].
///
/// Both limit-exceeded and genuine decode failures surface as
/// [`Error::Unsupported`]; this predicate lets the `--check` pass classify a
/// decompression-bomb guard trip (the stream is intact, merely larger than the
/// configured cap) as a warning rather than a stream-encoding error. The
/// sentinel is internal to flpdf — the trailing byte count is flpdf's own value —
/// so PDF content cannot forge a corrupt-stream message into this shape.
pub(crate) fn is_decode_output_limit_error(error: &Error) -> bool {
    matches!(error, Error::Unsupported(message) if message.starts_with(DECODE_OUTPUT_LIMIT_PREFIX))
}

/// Decode a stream's filter chain like [`decode_stream_data`], enforcing the
/// opt-in [`DecodeLimits`].
///
/// # Errors
///
/// Returns [`Error::Unsupported`] for the same reasons as [`decode_stream_data`],
/// plus when a supported filter stage's decoded output exceeds
/// [`DecodeLimits::max_output`], or when the `/Filter` chain exceeds
/// [`DecodeLimits::max_filter_chain`].
pub fn decode_stream_data_with_limits(
    dict: &Dictionary,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<Vec<u8>> {
    decode_stream_data_with_limits_and_warnings(
        dict,
        stream_data,
        limits,
        &mut reject_decode_warning,
    )
}

/// Encode `stream_data` by applying the stream dictionary's `/Filter` chain,
/// the inverse of [`decode_stream_data`].
///
/// Every PNG predictor encodes with the Up row filter, so `/Predictor 10`
/// through `/Predictor 15` produce identical output. The predictor number is
/// still recorded in the dictionary and the result decodes correctly, because
/// decoding selects a filter per row from the row's own leading byte. The TIFF
/// predictor (`/Predictor 2`) uses incremental horizontal differencing with the
/// same row geometry on encode and decode.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when:
/// - a `/Filter` entry is an unknown or unimplemented codec.
/// - `/Filter` is neither a name nor an array of names.
/// - `/DecodeParms` selects an unsupported `/Predictor` or an invalid row
///   geometry, on the same terms as [`decode_stream_data`].
pub fn encode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    encode_stream_data_with_filters(dict.get("Filter"), dict.get("DecodeParms"), stream_data)
}

/// Encode `stream_data` using `/Filter` and `/DecodeParms` read from an
/// `ObjectHandle` stream dictionary.
///
/// qpdf reads both keys through the resolving `stream_dict.getKey` accessor
/// (`libqpdf/QPDF_Stream.cc:386`, `:441`) and reads array children through
/// `getArrayItem` (`:400`, `:448`). `try_get_key` plus
/// `decode_filter_specs_from_handle` preserves that indirect-object behavior.
/// The encode pipeline remains the same one used by [`encode_stream_data`];
/// qpdf builds stream pipelines in reverse order and installs Flate deflate at
/// `libqpdf/QPDF_Stream.cc:529-568`. Predictor encoding remains qpdf's fixed
/// Up-row algorithm (`libqpdf/Pl_PNGFilter.cc:215-228`), and RunLength packet
/// plus EOD emission remains `libqpdf/Pl_RunLength.cc:105-145`.
///
/// # Errors
///
/// Returns the same filter and predictor errors as [`encode_stream_data`],
/// plus [`Error::Internal`] if an indirect holder or child still needs a
/// document resolver after its document has been dropped.
#[allow(dead_code)] // promoted when flpdf-egzr.3.2.5 migrates writer consumers
pub(crate) fn encode_stream_data_from_handle(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    let filter = stream_dict.try_get_key(b"/Filter")?;
    let decode_params = stream_dict.try_get_key(b"/DecodeParms")?;
    let specs = decode_filter_specs_from_handle(&filter, &decode_params, None)?;
    encode_stream_data_from_specs(specs, stream_data)
}

fn decode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_with_filters_and_crypt(
        filter,
        decode_params,
        stream_data,
        limits,
        data_events,
        &mut reject_crypt_stage,
    )
}

/// The crypt provider every non-decrypting decode entry point installs.
///
/// Plan decision D2 of `flpdf-25kg.3.4` keeps decryption out of this layer, so
/// a `Crypt` stage is recognised during staging and then refused here. Shared
/// by the legacy and `ObjectHandle` entry points, and the message itself is
/// [`CRYPT_STAGE_UNSUPPORTED`] so that this provider and the registry-side
/// `CryptStreamFilter::pipe_decode_recovering` report one definition rather
/// than one literal per route.
fn reject_crypt_stage(_decode_params: &DecodeParams, _data: &[u8]) -> Result<Vec<u8>> {
    Err(Error::Unsupported(CRYPT_STAGE_UNSUPPORTED.to_string()))
}

fn decode_stream_data_with_filters_and_crypt<F>(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
    decrypt_crypt: &mut F,
) -> Result<StreamDecodeOutcome>
where
    F: FnMut(&DecodeParams, &[u8]) -> Result<Vec<u8>>,
{
    let specs = decode_filter_specs_from_object(filter, decode_params, limits.max_filter_chain)?;
    decode_prepared_specs(specs, stream_data, limits, data_events, decrypt_crypt)
}

/// Decode a stream's data from its `ObjectHandle` stream dictionary — the
/// `ObjectHandle`-native counterpart of [`decode_stream_data_with_limits`].
///
/// `/Filter` and `/DecodeParms` are read off `stream_dict` the way
/// `QPDF_Stream::filterable` reads them, through `stream_dict.getKey`
/// (`libqpdf/QPDF_Stream.cc:386`, `:441`); `try_get_key` dereferences the
/// holder first, as qpdf's accessor does, and hands a missing key back as a
/// null handle. Every child is then inspected through the resolving `try_*`
/// accessors, so an indirect `/Filter` or `/DecodeParms` value is read as the
/// object it points at — see plan decision D1 of `flpdf-25kg.3.4`, whose
/// 2026-08-03 live-qpdf probe recorded that behavior.
///
/// `stream_data` is the stream's raw bytes; this entry point does not read
/// them out of the handle, because `flpdf-25kg.3.5` already holds them from
/// `readObjectAtOffset`.
///
/// # Errors
///
/// Returns [`Error::Unsupported`] on the same filter-chain, decode-parameter,
/// and codec conditions as [`decode_stream_data_with_limits`], and
/// [`Error::Internal`] when any handle resolved on this path — `stream_dict`
/// itself as well as a `/Filter` or `/DecodeParms` child — is indirect and its
/// document has been dropped (`ObjectHandle::try_dereference`).
///
/// On an unfilterable stream this matches `QPDF_Stream::getStreamData`'s
/// *outcome* only — qpdf throws there too (`QPDF_Stream.cc:350-357`). The
/// diagnostic channel still differs: qpdf emits `filterable`'s text as a
/// warning and throws a separate `"getStreamData called on unfilterable
/// stream"`, whereas flpdf emits no warning and raises `filterable`'s text as
/// the error itself. That gap is plan decision D3, measured against qpdf
/// 11.9.0 on 2026-08-03 and deliberately not closed here.
#[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
pub(crate) fn decode_stream_data_from_handle(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<Vec<u8>> {
    let outcome = decode_stream_data_from_handle_with_mode(
        stream_dict,
        stream_data,
        limits,
        DataEventMode::Suppress,
    )?;
    replay_strict_decode_outcome(outcome, &mut reject_decode_warning)
}

/// Decode a stream from its `ObjectHandle` stream dictionary while retaining
/// ordered recovery events — the `ObjectHandle`-native counterpart of
/// [`decode_stream_data_recovering_with_limits`].
///
/// [`decode_stream_data_from_handle`] reads the same dictionary through the
/// same private helper and then applies the legacy path's strict replay, so
/// the two differ only in that this form reports a warning or codec error as
/// an ordered event (alongside [`StreamDecodeEvent::Data`] chunks) where the
/// strict form turns the first of them into an [`Err`].
///
/// # Errors
///
/// Returns an outer [`Error`] when the filter chain cannot be read or
/// constructed, on the same terms as [`decode_stream_data_from_handle`].
/// Runtime codec failures instead remain ordered [`StreamDecodeEvent::Error`]
/// events alongside any recovered output.
#[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
pub(crate) fn decode_stream_data_recovering_from_handle(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_from_handle_with_mode(
        stream_dict,
        stream_data,
        limits,
        DataEventMode::Record,
    )
}

fn decode_stream_data_from_handle_with_mode(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
) -> Result<StreamDecodeOutcome> {
    let filter = stream_dict.try_get_key(b"/Filter")?;
    let decode_params = stream_dict.try_get_key(b"/DecodeParms")?;
    let specs = decode_filter_specs_from_handle(&filter, &decode_params, limits.max_filter_chain)?;
    decode_prepared_specs(
        specs,
        stream_data,
        limits,
        data_events,
        &mut reject_crypt_stage,
    )
}

/// The provider a decode entry point installs to handle a `Crypt` stage.
///
/// Erased rather than generic so the engine below stays one non-generic
/// function both shape readers call. Plan decision D2 of `flpdf-25kg.3.4`
/// keeps it an explicit parameter instead of a document hookup.
type CryptProvider<'a> = &'a mut dyn FnMut(&DecodeParams, &[u8]) -> Result<Vec<u8>>;

/// Run the staging, codec, and warning-ordering engine over already-read
/// filter specs.
///
/// Everything downstream of `FilterSpec` lives here in one copy: both shape
/// readers — the `&Object` one behind the legacy `&Dictionary` entry points
/// and the `ObjectHandle` one behind [`decode_stream_data_from_handle`] —
/// funnel into this function, so filter-chain staging, predictor geometry,
/// [`DecodeLimits::max_output`] enforcement, and event ordering cannot drift
/// between the two shapes. Nothing in this body inspects a `/Filter` or
/// `/DecodeParms` object of either shape.
///
/// [`DecodeLimits::max_filter_chain`] is the exception: it is applied above
/// this function, once per shape reader, so it *can* drift between them. The
/// shared `validate_filter_chain_count` keeps the message identical, and each
/// reader's own placement is pinned absolutely — by
/// `decode_rejects_overlong_filter_chain_before_malformed_item` here and by
/// `handle_reader_counts_the_raw_filter_array_before_inspecting_its_items` in
/// `stream_filter.rs`. What checks the two *against each other* is
/// `handle_reader_matches_object_reader_for_every_filter_shape`, which sweeps
/// the corpus at `None`, `Some(16)`, and `Some(0)`.
fn decode_prepared_specs(
    specs: Vec<FilterSpec>,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
    decrypt_crypt: CryptProvider<'_>,
) -> Result<StreamDecodeOutcome> {
    let prepared = prepare_decode_filters(specs)?;
    let stage_count = prepared.len();
    let mut decoded = Cow::Borrowed(stream_data);
    let mut events = Vec::new();
    let mut pending_events = Vec::new();
    let mut pending_data_boundary: Option<PendingDataBoundary> = None;
    let mut has_runtime_error = false;
    for (stage_index, mut stage) in prepared.into_iter().enumerate() {
        let is_last_stage = stage_index + 1 == stage_count;
        let next = match &mut stage.stage {
            PreparedStage::Crypt => {
                let data = decrypt_crypt(&stage.spec.decode_params, decoded.as_ref())?;
                append_final_crypt_events(
                    is_last_stage,
                    &mut events,
                    &data,
                    data_events,
                    pending_data_boundary,
                    &mut pending_events,
                );
                data
            }
            PreparedStage::Codec { adapter } => {
                let mut next_pending_data_boundary = pending_data_boundary.map(|boundary| {
                    let prefix_data = &decoded[..boundary.0];
                    let prefix = decode_codec_prefix(&stage.spec, prefix_data, limits);
                    let input_end = if boundary.1 {
                        prefix.data.len()
                    } else {
                        prefix.cleanup_data_start
                    };
                    PendingDataBoundary(input_end, boundary.1)
                });
                let mut stage_warnings = Vec::new();
                let outcome = adapter.pipe_decode_recovering(
                    decoded.as_ref(),
                    limits.max_output,
                    &mut |message, code, output_offset, phase| {
                        let ordinal = stage_warnings.len();
                        stage_warnings.push(PositionedDecodeEvent::local(
                            output_offset,
                            phase,
                            ordinal,
                            StreamDecodeEvent::Warning(StreamDecodeWarning {
                                message: message.to_string(),
                                code,
                            }),
                        ));
                        Ok(())
                    },
                )?;
                if let Some(stage_error) = outcome.error {
                    if has_runtime_error {
                        if is_last_stage {
                            let mut markers = stage_warnings;
                            if let Some(PendingDataBoundary(boundary, after_finish)) =
                                next_pending_data_boundary
                            {
                                markers.extend(position_pending_events(
                                    boundary,
                                    after_finish,
                                    std::mem::take(&mut pending_events),
                                ));
                            }
                            append_positioned_events(
                                &mut events,
                                &outcome.data,
                                data_events,
                                markers,
                            );
                        } else {
                            append_plain_events(&mut events, stage_warnings);
                        }
                    } else {
                        has_runtime_error = true;
                        let error_phase = if stage_error.during_write {
                            FilterDecodePhase::Write
                        } else {
                            FilterDecodePhase::Finish
                        };
                        let error_offset = stage_error.output_offset;
                        let error_ordinal = stage_warnings.len();
                        stage_warnings.push(PositionedDecodeEvent::local(
                            error_offset,
                            error_phase,
                            error_ordinal,
                            StreamDecodeEvent::Error(stage_error.error),
                        ));
                        if !is_last_stage {
                            next_pending_data_boundary =
                                Some(PendingDataBoundary(error_offset, !stage_error.during_write));
                            pending_events.extend(positioned_into_plain_events(stage_warnings));
                        } else {
                            if let Some(PendingDataBoundary(boundary, after_finish)) =
                                next_pending_data_boundary
                            {
                                stage_warnings.extend(position_pending_events(
                                    boundary,
                                    after_finish,
                                    std::mem::take(&mut pending_events),
                                ));
                            }
                            append_positioned_events(
                                &mut events,
                                &outcome.data,
                                data_events,
                                stage_warnings,
                            );
                        }
                    }
                } else {
                    if is_last_stage {
                        let mut markers = stage_warnings;
                        if let Some(PendingDataBoundary(boundary, after_finish)) =
                            next_pending_data_boundary
                        {
                            markers.extend(position_pending_events(
                                boundary,
                                after_finish,
                                std::mem::take(&mut pending_events),
                            ));
                        }
                        append_positioned_events(&mut events, &outcome.data, data_events, markers);
                    } else if !stage_warnings.is_empty() {
                        let boundary = stage_warnings[0].offset;
                        pending_events.extend(positioned_into_plain_events(stage_warnings));
                        next_pending_data_boundary = Some(PendingDataBoundary(boundary, false));
                    }
                }
                if !is_last_stage && (has_runtime_error || next_pending_data_boundary.is_some()) {
                    pending_data_boundary = next_pending_data_boundary;
                }
                outcome.data
            }
        };
        decoded = Cow::Owned(next);
    }
    let data = decoded.into_owned();
    if stage_count == 0 {
        data_events.push(&mut events, &data);
    }
    Ok(StreamDecodeOutcome { data, events })
}

/// A filter-chain stage with its decode route already resolved.
///
/// Every name flpdf decodes resolves to a registered `StreamFilter`;
/// `Crypt` is the one stage the caller decrypts instead.
enum PreparedStage {
    /// The stage's `/DecodeParms` stay on `PreparedDecodeFilter::spec`, where
    /// the crypt provider reads them.
    Crypt,
    Codec {
        adapter: Box<dyn crate::stream_filter::StreamFilter>,
    },
}

struct PreparedDecodeFilter {
    spec: FilterSpec,
    stage: PreparedStage,
}

fn prepare_decode_filters(specs: Vec<FilterSpec>) -> Result<Vec<PreparedDecodeFilter>> {
    let mut prepared = Vec::with_capacity(specs.len());
    for spec in specs {
        let filter_name = spec.normalized_name();
        if filter_name == b"Crypt" {
            prepared.push(PreparedDecodeFilter {
                spec,
                stage: PreparedStage::Crypt,
            });
            continue;
        }

        let Some(mut adapter) = stream_filter_for(filter_name) else {
            return Err(undecodable_filter_error(filter_name));
        };
        if !adapter.set_decode_params(&spec.decode_params) {
            return Err(Error::Unsupported(format!(
                "stream filter {} does not support supplied /DecodeParms",
                String::from_utf8_lossy(filter_name)
            )));
        }

        prepared.push(PreparedDecodeFilter {
            spec,
            stage: PreparedStage::Codec { adapter },
        });
    }

    // QPDF_Stream::pipeStreamData constructs the whole chain before piping any
    // data, walking the filters in reverse, so a later stage with unusable
    // parameters is reported even when an earlier stage would fail on the data.
    for stage in prepared.iter().rev() {
        if let PreparedStage::Codec { adapter } = &stage.stage {
            adapter.preflight_decode_pipeline()?;
        }
    }
    Ok(prepared)
}

#[derive(Clone, Copy)]
struct PendingDataBoundary(usize, bool);

struct PositionedDecodeEvent {
    offset: usize,
    barrier: u8,
    ordinal: usize,
    event: StreamDecodeEvent,
}

fn append_final_crypt_events(
    is_last_stage: bool,
    events: &mut Vec<StreamDecodeEvent>,
    data: &[u8],
    data_events: DataEventMode,
    pending_data_boundary: Option<PendingDataBoundary>,
    pending_events: &mut Vec<StreamDecodeEvent>,
) {
    if !is_last_stage {
        return;
    }
    let mut markers = Vec::new();
    if let Some(PendingDataBoundary(boundary, after_finish)) = pending_data_boundary {
        markers.extend(position_pending_events(
            boundary,
            after_finish,
            std::mem::take(pending_events),
        ));
    }
    append_positioned_events(events, data, data_events, markers);
}

impl PositionedDecodeEvent {
    fn local(
        offset: usize,
        phase: FilterDecodePhase,
        ordinal: usize,
        event: StreamDecodeEvent,
    ) -> Self {
        let barrier = match phase {
            FilterDecodePhase::Write => 1,
            FilterDecodePhase::Finish => 2,
        };
        Self {
            offset,
            barrier,
            ordinal,
            event,
        }
    }
}

fn position_pending_events(
    offset: usize,
    after_finish: bool,
    events: Vec<StreamDecodeEvent>,
) -> impl Iterator<Item = PositionedDecodeEvent> {
    // A pending upstream event at this boundary happens before the downstream
    // stage consumes any cleanup bytes that follow it. Those bytes can produce
    // a write-time event at the same output offset, so the pending event must
    // win that tie. An event explicitly marked after finish remains last.
    let barrier = if after_finish { 3 } else { 0 };
    events
        .into_iter()
        .enumerate()
        .map(move |(ordinal, event)| PositionedDecodeEvent {
            offset,
            barrier,
            ordinal,
            event,
        })
}

fn sort_positioned_events(events: &mut [PositionedDecodeEvent]) {
    events.sort_by_key(|event| (event.offset, event.barrier, event.ordinal));
}

fn positioned_into_plain_events(
    mut events: Vec<PositionedDecodeEvent>,
) -> impl Iterator<Item = StreamDecodeEvent> {
    sort_positioned_events(&mut events);
    events.into_iter().map(|event| event.event)
}

fn append_plain_events(output: &mut Vec<StreamDecodeEvent>, events: Vec<PositionedDecodeEvent>) {
    output.extend(positioned_into_plain_events(events));
}

fn append_positioned_events(
    output: &mut Vec<StreamDecodeEvent>,
    data: &[u8],
    data_events: DataEventMode,
    mut events: Vec<PositionedDecodeEvent>,
) {
    sort_positioned_events(&mut events);
    let mut data_start = 0;
    for event in events {
        let offset = event.offset.min(data.len()).max(data_start);
        data_events.push(output, &data[data_start..offset]);
        output.push(event.event);
        data_start = offset;
    }
    data_events.push(output, &data[data_start..]);
}

fn decode_codec_prefix(
    spec: &FilterSpec,
    data: &[u8],
    limits: DecodeLimits,
) -> crate::stream_filter::FilterDecodeOutcome {
    let filter_name = spec.normalized_name();
    let mut adapter =
        stream_filter_for(filter_name).expect("a prepared codec has a registered prefix decoder");
    // `debug_assert!` evaluates its expression only when debug assertions are
    // on, so applying the parameters *inside* the assertion silently skipped
    // the predictor in release builds and produced a different prefix length,
    // a different event boundary, and ultimately a different public error.
    let applied = adapter.set_decode_params(&spec.decode_params);
    debug_assert!(applied);
    adapter
        .pipe_decode_recovering(data, limits.max_output, &mut |_, _, _, _| Ok(()))
        .expect("preflighted codec prefix pipeline is infallible")
}

fn encode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    // The encode path is writer output rather than untrusted input, so it is
    // uncapped — see `MAX_FILTER_CHAIN_LEN`'s own doc.
    let specs = decode_filter_specs_from_object(filter, decode_params, None)?;
    encode_stream_data_from_specs(specs, stream_data)
}

fn encode_stream_data_from_specs(specs: Vec<FilterSpec>, stream_data: &[u8]) -> Result<Vec<u8>> {
    // ISO 32000-1 §7.4.2: the /Filter array names filters in *decode*
    // order, so encoding must apply them in reverse for round-tripping.
    let mut encoded = stream_data.to_vec();
    for spec in specs.into_iter().rev() {
        let after_predictor =
            apply_encode_params(spec.normalized_name(), &spec.decode_params, &encoded)?;
        encoded = if spec.normalized_name() == b"FlateDecode" {
            encode_flate(&after_predictor)?
        } else {
            apply_single_filter_encode(spec.normalized_name(), &after_predictor)
                .map_err(Error::Unsupported)?
        };
    }
    Ok(encoded)
}

/// Apply the predictor selected by `/DecodeParms`, if any, and validate the
/// target filter's DecodeParms contract before encoding.
fn apply_encode_params(
    filter_name: &[u8],
    decode_params: &DecodeParams,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    crate::stream_filter::encode_predictor(stream_data, filter_name, decode_params)
}

/// Apply a single encode filter to `stream_data`.
///
/// # Write-side compression policy
///
/// flpdf writes stream compression as **FlateDecode only**.
/// LZWEncode is intentionally unsupported — qpdf also has no LZW encoder.
/// Image/binary passthrough codecs (DCTDecode, JBIG2Decode, JPXDecode, CCITTFaxDecode)
/// are never re-encoded by flpdf; the writer preserves those streams verbatim.
fn apply_single_filter_encode(
    filter_name: &[u8],
    stream_data: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    if filter_name == b"ASCII85Decode" {
        return Err(
            "ASCII85Encode is not supported: qpdf provides an ASCII85 decoder but no encoder"
                .to_string(),
        );
    }

    if filter_name == b"ASCIIHexDecode" {
        return Ok(ascii_hex::encode(stream_data));
    }

    if filter_name == b"RunLengthDecode" {
        return encode_run_length(stream_data).map_err(|error| error.to_string());
    }

    // LZWEncode is not supported: flpdf writes stream compression as FlateDecode only
    // (qpdf has no LZW encoder either).
    if filter_name == b"LZWDecode" {
        return Err(
            "LZWEncode is not supported: flpdf writes stream compression as FlateDecode only \
             (qpdf has no LZW encoder either)"
                .to_string(),
        );
    }

    // Passthrough codecs are never re-encoded; the writer preserves those streams verbatim.
    if let Some(label) = passthrough_codec_label(filter_name) {
        return Err(format!(
            "encode not supported for passthrough codec {label}: \
             image/binary streams are preserved verbatim by flpdf"
        ));
    }

    Err(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_handle::identity_tests::resolver_bearing_handle;
    use crate::object_handle::warning_emission_tests::{handle_resolving, WarningRecorder};
    use crate::object_handle::ObjectValue;
    use crate::pipeline::lzw::pack_codes;
    use crate::stream_filter::{tests::handle_from_object, ParamValue};
    use std::rc::Rc;

    #[test]
    fn decode_limits_default_to_unbounded_output_and_sixteen_filters() {
        assert_eq!(
            DecodeLimits::default(),
            DecodeLimits {
                max_output: None,
                max_filter_chain: Some(16),
            }
        );
    }

    #[test]
    fn unlimited_chain_policy_reaches_filter_item_validation() {
        let mut filters = vec![Object::Name(b"ASCIIHexDecode".to_vec()); 16];
        filters.push(Object::Integer(1));
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter", Object::Array(filters));

        let error = decode_stream_data_recovering_with_limits(
            &dictionary,
            b">",
            DecodeLimits {
                max_output: None,
                max_filter_chain: None,
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    fn flate_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict
    }

    fn valid_prefix_then_invalid_stored_block() -> (Vec<u8>, Vec<u8>) {
        let mut encoded = vec![
            0x78, 0x01, // zlib header
            0x00, 0xff, 0xff, 0x00, 0x00, // non-final stored block, 65,535 bytes
        ];
        encoded.extend(std::iter::repeat_n(b'A', 65_535));
        encoded.extend_from_slice(&[
            0x00, 0x02, 0x00, 0xfd, 0xff, b'B', b'C', // valid non-final two-byte block
            0x01, 0x01, 0x00, 0x00, 0x00, // final block with invalid LEN/NLEN
        ]);

        let mut decoded = vec![b'A'; 65_535];
        decoded.extend_from_slice(b"BC");
        (encoded, decoded)
    }

    #[test]
    fn decode_stream_data_accepts_qpdf_flate_abbreviation() {
        let encoded = encode_stream_data(&flate_dict(), b"abbreviated filter").unwrap();
        let mut abbreviated = Dictionary::new();
        abbreviated.insert("Filter", Object::Name(b"Fl".to_vec()));

        let decoded = decode_stream_data(&abbreviated, &encoded).unwrap();

        assert_eq!(decoded, b"abbreviated filter");
    }

    #[test]
    fn first_filter_borrows_the_callers_encoded_input() {
        let input = b"borrowed first-stage input";
        expect_first_filter_input(input);
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"TestBorrowedInput".to_vec()));

        assert_eq!(decode_stream_data(&dict, input).unwrap(), input);
    }

    #[test]
    fn recovering_decode_propagates_a_post_preflight_adapter_error() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"TestPostPreflightFailure".to_vec()));

        let error = decode_stream_data_recovering(&dict, b"encoded input").unwrap_err();

        assert_eq!(error.to_string(), "test post-preflight decode failure");
    }

    #[test]
    fn decode_stream_data_rejects_misaligned_decode_parms_before_codec_runs() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"ASCII85Decode".to_vec()),
            ]),
        );
        dict.insert("DecodeParms", Object::Array(vec![Object::Null]));

        let error = decode_stream_data(&dict, b"not zlib").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
        );
    }

    #[test]
    fn decode_stream_data_exposes_plflate_malformed_header_timing() {
        let error = decode_stream_data(&flate_dict(), b"\x78\x00").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream inflate: inflate: data: incorrect header check"
        );
    }

    #[test]
    fn recovering_decode_retains_partial_bytes_after_codec_error() {
        let (encoded, decoded_prefix) = valid_prefix_then_invalid_stored_block();

        let outcome = decode_stream_data_recovering(&flate_dict(), &encoded).unwrap();

        assert_eq!(outcome.data, decoded_prefix[..65_536]);
        assert_eq!(outcome.events.len(), 2);
        assert!(matches!(
            &outcome.events[0],
            StreamDecodeEvent::Data(data) if data == &decoded_prefix[..65_536]
        ));
        assert!(matches!(
            &outcome.events[1],
            StreamDecodeEvent::Error(error)
                if error
                    .to_string()
                    .starts_with("unsupported PDF feature: stream inflate: inflate: data:")
        ));
    }

    #[test]
    fn recovering_decode_reports_blank_partial_for_malformed_header() {
        let outcome = decode_stream_data_recovering(&flate_dict(), b"abc").unwrap();

        assert!(outcome.data.is_empty());
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            &outcome.events[0],
            StreamDecodeEvent::Error(error)
                if error.to_string()
                    == "unsupported PDF feature: stream inflate: inflate: data: incorrect header check"
        ));
    }

    #[test]
    fn strict_decode_keeps_the_recovering_codec_error_contract() {
        let (encoded, _) = valid_prefix_then_invalid_stored_block();
        let outcome = decode_stream_data_recovering(&flate_dict(), &encoded).unwrap();
        let strict_error = decode_stream_data(&flate_dict(), &encoded).unwrap_err();

        assert!(outcome.events.iter().any(|event| {
            matches!(event, StreamDecodeEvent::Error(recovering_error)
                if strict_error.to_string() == recovering_error.to_string())
        }));
    }

    #[test]
    fn strict_decode_retains_data_without_recording_a_duplicate_data_event() {
        let encoded = encode_stream_data(&flate_dict(), b"strict payload").unwrap();
        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &flate_dict(),
            &encoded,
            DecodeLimits::default(),
            DataEventMode::Suppress,
        )
        .unwrap();

        assert_eq!(outcome.data, b"strict payload");
        assert!(!outcome
            .events
            .iter()
            .any(|event| matches!(event, StreamDecodeEvent::Data(_))));
    }

    #[test]
    fn strict_decode_without_filters_suppresses_the_passthrough_data_event() {
        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &Dictionary::new(),
            b"unfiltered payload",
            DecodeLimits::default(),
            DataEventMode::Suppress,
        )
        .unwrap();

        assert_eq!(outcome.data, b"unfiltered payload");
        assert!(outcome.events.is_empty());
    }

    #[test]
    fn recovering_decode_retains_only_the_first_filter_runtime_error() {
        let dict = array_filter_dict(&[b"ASCIIHexDecode", b"ASCIIHexDecode"]);

        let outcome = decode_stream_data_recovering(&dict, b"4G").unwrap();

        assert!(outcome.data.is_empty());
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            &outcome.events[0],
            StreamDecodeEvent::Error(error)
                if error.to_string()
                    == "unsupported PDF feature: character out of range during base Hex decode: G"
        ));
    }

    #[test]
    fn recovering_decode_collects_nonfatal_flate_warnings() {
        let outcome = decode_stream_data_recovering(&flate_dict(), b"\x78").unwrap();

        assert!(outcome.data.is_empty());
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            &outcome.events[0],
            StreamDecodeEvent::Warning(warning)
                if warning.message == "input stream is complete but output may still be valid"
                    && warning.code == -5
        ));
    }

    #[test]
    fn recovering_final_flate_warning_precedes_predictor_finish_data() {
        let mut encoded = encode_flate(b"\0A").unwrap();
        encoded.truncate(encoded.len() - 4);

        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(12));
        decode_params.insert("Columns", Object::Integer(2));
        let mut dictionary = flate_dict();
        dictionary.insert("DecodeParms", Object::Dictionary(decode_params));

        let outcome = decode_stream_data_recovering(&dictionary, &encoded).unwrap();

        assert_eq!(outcome.data, b"A\0");
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Warning(warning),
                StreamDecodeEvent::Data(data),
            ] if warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
                && data == b"A\0"
        ));
    }

    #[test]
    fn recovering_final_flate_warning_precedes_predictor_finish_limit() {
        let mut encoded = encode_flate(b"\0A").unwrap();
        encoded.truncate(encoded.len() - 4);

        let mut decode_params = Dictionary::new();
        decode_params.insert("Predictor", Object::Integer(12));
        decode_params.insert("Columns", Object::Integer(2));
        let mut dictionary = flate_dict();
        dictionary.insert("DecodeParms", Object::Dictionary(decode_params));

        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &dictionary,
            &encoded,
            DecodeLimits {
                max_output: Some(1),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
        )
        .unwrap();

        assert_eq!(outcome.data, b"A");
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Warning(warning),
                StreamDecodeEvent::Data(data),
                StreamDecodeEvent::Error(error),
            ] if warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
                && data == b"A"
                && error.to_string()
                    == "unsupported PDF feature: decoded output exceeds configured limit of 1 bytes"
        ));
    }

    #[test]
    fn recovering_pending_error_precedes_equal_offset_final_finish_warning() {
        let filter = Object::Array(vec![
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"FlateDecode".to_vec()),
        ]);
        let mut predictor = Dictionary::new();
        predictor.insert("Predictor", Object::Integer(12));
        predictor.insert("Columns", Object::Integer(2));
        let decode_params = Object::Array(vec![Object::Null, Object::Dictionary(predictor)]);
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter", filter);
        dictionary.insert("DecodeParms", decode_params);

        let outcome = decode_stream_data_recovering(&dictionary, b"789C63700400G").unwrap();

        assert_eq!(outcome.data, b"A\0");
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Error(error),
                StreamDecodeEvent::Warning(warning),
                StreamDecodeEvent::Data(data),
            ] if error.to_string()
                    == "unsupported PDF feature: character out of range during base Hex decode: G"
                && warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
                && data == b"A\0"
        ));
    }

    /// flpdf-4rfl: the prefix probe must apply `/DecodeParms` in every build
    /// profile. Applying them inside `debug_assert!` skipped the call whenever
    /// debug assertions were off, so the probe decoded with default geometry,
    /// `PendingDataBoundary` landed elsewhere, and `decode_stream_data`
    /// returned a different error in release than in debug.
    #[test]
    fn codec_prefix_probe_applies_decode_params_in_every_build_profile() {
        let spec = FilterSpec {
            name: b"FlateDecode".to_vec(),
            decode_params: DecodeParams::Present(vec![
                (b"Predictor".to_vec(), ParamValue::Int(12)),
                (b"Columns".to_vec(), ParamValue::Int(4)),
            ]),
        };
        // One Up-filtered PNG row: without the predictor the probe emits the
        // five encoded bytes instead of the four predicted ones.
        let encoded = encode_flate(&[2, 0x01, 0x02, 0x03, 0x04]).unwrap();

        let outcome = decode_codec_prefix(&spec, &encoded, DecodeLimits::default());

        assert_eq!(outcome.data, [0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn strict_replay_ignores_a_defensive_data_event() {
        let error = replay_strict_decode_event(
            StreamDecodeEvent::Data(b"synthetic recovered data".to_vec()),
            &mut reject_decode_warning,
        );

        assert!(error.is_none());
    }

    #[test]
    fn strict_replay_delivers_warning_after_error_and_keeps_the_runtime_error() {
        let dict = array_filter_dict(&[b"ASCIIHexDecode", b"FlateDecode"]);
        let mut warnings = Vec::new();

        let error = decode_stream_data_with_limits_and_warnings(
            &dict,
            b"78G",
            DecodeLimits::default(),
            &mut |message, code| {
                warnings.push((message.to_string(), code));
                Err(PipelineError::runtime("later callback failure"))
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: character out of range during base Hex decode: G"
        );
        assert_eq!(
            warnings,
            vec![(
                "input stream is complete but output may still be valid".to_string(),
                -5
            )]
        );
    }

    #[test]
    fn strict_replay_keeps_a_callback_error_and_delivers_later_warnings() {
        let mut encoded = encode_stream_data(&flate_dict(), b"78G").unwrap();
        encoded.pop();
        let dict = array_filter_dict(&[b"FlateDecode", b"ASCIIHexDecode", b"FlateDecode"]);
        let mut warnings = Vec::new();

        let error = decode_stream_data_with_limits_and_warnings(
            &dict,
            &encoded,
            DecodeLimits::default(),
            &mut |message, code| {
                warnings.push((message.to_string(), code));
                Err(PipelineError::runtime(format!(
                    "callback failure {}",
                    warnings.len()
                )))
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: callback failure 1"
        );
        assert_eq!(
            warnings,
            vec![
                (
                    "input stream is complete but output may still be valid".to_string(),
                    -5
                ),
                (
                    "input stream is complete but output may still be valid".to_string(),
                    -5
                ),
            ]
        );
    }

    #[test]
    fn recovering_decode_records_finish_time_output_limit_after_partial_input() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));

        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &dict,
            b"F",
            DecodeLimits {
                max_output: Some(0),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
        )
        .unwrap();

        assert!(outcome.data.is_empty());
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            &outcome.events[0],
            StreamDecodeEvent::Error(error)
                if error.to_string()
                    == "unsupported PDF feature: decoded output exceeds configured limit of 0 bytes"
        ));
    }

    #[test]
    fn recovering_decode_emits_data_before_a_finish_time_error() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));

        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &dict,
            b"41F",
            DecodeLimits {
                max_output: Some(1),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
        )
        .unwrap();

        assert_eq!(outcome.data, b"A");
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Data(data), StreamDecodeEvent::Error(error)]
                if data == b"A"
                    && error.to_string()
                        == "unsupported PDF feature: decoded output exceeds configured limit of 1 bytes"
        ));
    }

    #[test]
    fn recovering_decode_defers_a_nonfinal_finish_time_error_until_after_data() {
        let dict = array_filter_dict(&[b"ASCIIHexDecode", b"ASCIIHexDecode"]);

        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &dict,
            b"41F",
            DecodeLimits {
                max_output: Some(1),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
        )
        .unwrap();

        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Data(data), StreamDecodeEvent::Error(error)]
                if data == b"\xa0"
                    && error.to_string()
                        == "unsupported PDF feature: decoded output exceeds configured limit of 1 bytes"
        ));
    }

    #[test]
    fn recovering_decode_keeps_final_data_after_a_prior_runtime_error() {
        let dict = array_filter_dict(&[b"FlateDecode", b"ASCIIHexDecode"]);
        let (mut compressed, _) = valid_prefix_then_invalid_stored_block();
        compressed[7..10].copy_from_slice(b"41G");

        let outcome = decode_stream_data_recovering(&dict, &compressed).unwrap();

        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Data(data), StreamDecodeEvent::Error(error)]
                if data == b"A"
                    && error.to_string()
                        .starts_with("unsupported PDF feature: stream inflate: inflate: data:")
        ));
    }

    #[test]
    fn recovering_decode_keeps_downstream_cleanup_after_upstream_write_error() {
        let dict = array_filter_dict(&[b"ASCIIHexDecode", b"ASCIIHexDecode"]);

        let outcome = decode_stream_data_recovering(&dict, b"343G").unwrap();

        assert_eq!(outcome.data, b"@");
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Error(error), StreamDecodeEvent::Data(data)]
                if error.to_string()
                    == "unsupported PDF feature: character out of range during base Hex decode: G"
                    && data == b"@"
        ));
    }

    #[test]
    fn recovering_decode_replays_final_cleanup_after_two_write_errors() {
        let filter = Object::Array(vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"ASCIIHexDecode".to_vec()),
        ]);
        let mut encoded = ascii85_fixture_bytes(b"4   ");
        encoded.truncate(encoded.len() - 2);
        let cleanup = ascii85_fixture_bytes(b"G");
        encoded.extend_from_slice(&cleanup[..2]);
        encoded.push(b'z');
        let mut decrypt = |_: &DecodeParams, data: &[u8]| Ok(data.to_vec());

        let outcome = decode_stream_data_with_filters_and_crypt(
            Some(&filter),
            None,
            &encoded,
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .unwrap();

        assert_eq!(outcome.data, b"@");
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Error(error), StreamDecodeEvent::Data(data)]
                if error.to_string() == "unsupported PDF feature: unexpected z during base 85 decode"
                    && data == b"@"
        ));
    }

    #[test]
    fn recovering_decode_preserves_prefix_probe_and_intermediate_error_paths() {
        let mut decrypt = |_: &DecodeParams, data: &[u8]| Ok(data.to_vec());
        let two_ascii_hex = Object::Array(vec![
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"ASCIIHexDecode".to_vec()),
        ]);

        let outcome = decode_stream_data_with_filters_and_crypt(
            Some(&two_ascii_hex),
            None,
            b"47G",
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .unwrap();
        assert!(outcome.data.is_empty());
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Error(error)]
                if error.to_string()
                    == "unsupported PDF feature: character out of range during base Hex decode: G"
        ));

        let finish_outcome = decode_stream_data_with_filters_and_crypt(
            Some(&two_ascii_hex),
            None,
            b"41F",
            DecodeLimits {
                max_output: Some(1),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
            &mut decrypt,
        )
        .unwrap();
        assert!(matches!(
            &finish_outcome.events[..],
            [StreamDecodeEvent::Data(data), StreamDecodeEvent::Error(error)]
                if data == b"\xa0"
                    && error.to_string()
                        == "unsupported PDF feature: decoded output exceeds configured limit of 1 bytes"
        ));

        let three_stages = Object::Array(vec![
            Object::Name(b"ASCII85Decode".to_vec()),
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"TestRejectDecode".to_vec()),
        ]);
        let mut encoded = ascii85_fixture_bytes(b"4   ");
        encoded.truncate(encoded.len() - 2);
        let cleanup = ascii85_fixture_bytes(b"G");
        encoded.extend_from_slice(&cleanup[..2]);
        encoded.push(b'z');

        let outcome = decode_stream_data_with_filters_and_crypt(
            Some(&three_stages),
            None,
            &encoded,
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .unwrap();
        assert_eq!(outcome.data, b"@");
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Error(error), StreamDecodeEvent::Data(data)]
                if error.to_string() == "unsupported PDF feature: unexpected z during base 85 decode"
                    && data == b"@"
        ));
    }

    #[test]
    fn recovering_decode_preserves_chunked_prefix_before_a_later_output_limit() {
        let mut state = 0x1234_5678u32;
        let mut plain = Vec::with_capacity(102_500);
        for _ in 0..2_500 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            plain.push((state >> 24) as u8);
        }
        plain.extend(std::iter::repeat_n(b'A', 100_000));
        let inner_flate = encode_flate(&plain).unwrap();
        assert!((2_500..4_000).contains(&inner_flate.len()));

        // The first complete predictor row contains only the incompressible
        // prefix. Flate warns before the predictor finishes the partial second
        // row, whose remaining compressed bytes expand beyond the downstream
        // limit.
        let columns = inner_flate.len().div_ceil(2);
        let mut predicted = Vec::with_capacity(inner_flate.len() + 2);
        predicted.push(0);
        predicted.extend_from_slice(&inner_flate[..columns]);
        predicted.push(0);
        predicted.extend_from_slice(&inner_flate[columns..inner_flate.len() - 1]);

        let mut outer_flate = encode_flate(&predicted).unwrap();
        outer_flate.truncate(outer_flate.len() - 4);

        let mut predictor = Dictionary::new();
        predictor.insert("Predictor", Object::Integer(12));
        predictor.insert("Columns", Object::Integer(columns as i64));
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Dictionary(predictor), Object::Null]),
        );

        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &dict,
            &outer_flate,
            DecodeLimits {
                max_output: Some(4_000),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
        )
        .unwrap();

        assert_eq!(outcome.data.len(), 4_000);
        assert_eq!(outcome.data, plain[..4_000]);
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Data(before_warning),
                StreamDecodeEvent::Warning(warning),
                StreamDecodeEvent::Data(after_warning),
                StreamDecodeEvent::Error(error),
                StreamDecodeEvent::Warning(inner_warning),
            ] if !before_warning.is_empty()
                && !after_warning.is_empty()
                && before_warning.len() + after_warning.len() == outcome.data.len()
                && before_warning
                    .iter()
                    .chain(after_warning)
                    .copied()
                    .eq(outcome.data.iter().copied())
                && warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
                && error.to_string()
                    == "unsupported PDF feature: decoded output exceeds configured limit of 4000 bytes"
                && inner_warning.message
                    == "input stream is complete but output may still be valid"
                && inner_warning.code == -5
        ));
        assert_eq!(
            decode_stream_data_with_limits(
                &dict,
                &outer_flate,
                DecodeLimits {
                    max_output: Some(4_000),
                    ..DecodeLimits::default()
                },
            )
            .unwrap_err()
            .to_string(),
            "unsupported PDF feature: stream inflate: input stream is complete but output may still be valid (zlib error -5)"
        );
    }

    #[test]
    fn recovering_decode_replays_nonfinal_predictor_warning_before_final_lzw_finish_limit() {
        let lzw = pack_codes(&[256, 0], true);
        assert_eq!(lzw.len(), 3);

        // The first complete row leaves LZW with only its Clear code. The
        // predictor's finish-time tail supplies the last bit of the literal;
        // LZW forwards that byte to its predictor, whose own finish pads a
        // partial row and crosses the per-stage output limit.
        let mut predicted = vec![0];
        predicted.extend_from_slice(&lzw[..2]);
        predicted.push(0);
        predicted.push(lzw[2]);
        let mut encoded = encode_flate(&predicted).unwrap();
        encoded.truncate(encoded.len() - 4);

        let mut first_predictor = Dictionary::new();
        first_predictor.insert("Predictor", Object::Integer(12));
        first_predictor.insert("Columns", Object::Integer(2));
        let mut final_predictor = Dictionary::new();
        final_predictor.insert("Predictor", Object::Integer(12));
        final_predictor.insert("Columns", Object::Integer(5));
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"LZWDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![
                Object::Dictionary(first_predictor),
                Object::Dictionary(final_predictor),
            ]),
        );

        let outcome = decode_stream_data_recovering_with_limits_and_mode(
            &dict,
            &encoded,
            DecodeLimits {
                max_output: Some(4),
                ..DecodeLimits::default()
            },
            DataEventMode::Record,
        )
        .unwrap();

        assert_eq!(outcome.data, [0; 4]);
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Warning(warning),
                StreamDecodeEvent::Data(data),
                StreamDecodeEvent::Error(error),
            ] if warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
                && data == b"\0\0\0\0"
                && error.to_string()
                    == "unsupported PDF feature: decoded output exceeds configured limit of 4 bytes"
        ));
    }

    #[test]
    fn recovering_decode_keeps_filterability_failures_in_outer_result() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"BogusDecode".to_vec()));

        let error = decode_stream_data_recovering(&dict, b"abc").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: unsupported stream filter: BogusDecode"
        );
    }

    #[test]
    fn decode_stream_data_rejects_truncated_flate_warning() {
        let error = decode_stream_data(&flate_dict(), b"\x78").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream inflate: \
             input stream is complete but output may still be valid (zlib error -5)"
        );
    }

    #[test]
    fn invalid_flate_decode_params_fail_before_malformed_stream_data() {
        let mut params = Dictionary::new();
        params.insert("Predictor", Object::Integer(9));
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(params));

        let error = decode_stream_data(&dict, b"not deflate data").unwrap_err();

        // A predictor outside {1, 2, 10..=15} makes the stream unfilterable, so
        // the rejection precedes any attempt to inflate the body.
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter FlateDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn registered_filter_rejects_unsupported_decode_params_before_pipeline() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"TestRejectDecode".to_vec()));
        dict.insert("DecodeParms", Object::Integer(1));

        let error = decode_stream_data(&dict, b"not decoded").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter TestRejectDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn invalid_later_filter_params_win_before_malformed_earlier_codec_data() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"RunLengthDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Dictionary(Dictionary::new())]),
        );

        let error = decode_stream_data(&dict, b"~X").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter RunLengthDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn invalid_later_filter_params_win_before_earlier_expansion_limit() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"RunLengthDecode".to_vec()),
                Object::Name(b"ASCIIHexDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Dictionary(Dictionary::new())]),
        );

        let error = decode_stream_data_with_limits(
            &dict,
            &[0xf9, b'A'],
            DecodeLimits {
                max_output: Some(0),
                ..DecodeLimits::default()
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter ASCIIHexDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn invalid_later_filter_params_prevent_earlier_codec_warnings() {
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"ASCIIHexDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Dictionary(Dictionary::new())]),
        );
        let error = decode_stream_data_with_limits_and_warnings(
            &dict,
            b"\x78",
            DecodeLimits::default(),
            &mut reject_decode_warning,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter ASCIIHexDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn decode_stream_data_without_decryption_keeps_plaintext_behavior() {
        let dict = flate_dict();
        let plaintext = b"legacy plaintext flate";
        let encoded = encode_stream_data(&dict, plaintext).unwrap();

        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext);
    }

    // ----- ASCIIHexDecode filter integration tests -----

    fn ascii_hex_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
        dict
    }

    #[test]
    fn decode_stream_data_ascii_hex_round_trip() {
        let dict = ascii_hex_dict();
        let plaintext = b"Hello from ASCIIHexDecode filter!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii_hex_empty() {
        let dict = ascii_hex_dict();
        let plaintext = b"";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii_hex_odd_length_data() {
        let dict = ascii_hex_dict();
        // 3 bytes → odd nibble count in inner encoding only if we provide raw odd data;
        // encode always emits two hex chars per byte so no padding needed on decode
        let plaintext = b"ABC";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii_hex_uses_qpdf_whitespace_and_error_semantics() {
        assert_eq!(
            decode_stream_data(&ascii_hex_dict(), b"4\x0b142>").unwrap(),
            b"AB"
        );

        let error = decode_stream_data(&ascii_hex_dict(), b"41\0").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: character out of range during base Hex decode: "
        );
    }

    // ----- ASCII85Decode filter integration tests -----

    fn ascii85_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        dict
    }

    #[test]
    fn decode_stream_data_ascii85_round_trip() {
        let dict = ascii85_dict();
        let plaintext = b"Hello from ASCII85Decode filter!";

        let encoded = ascii85_fixture_bytes(plaintext);
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn encode_stream_data_ascii85_is_explicitly_unsupported() {
        let dict = ascii85_dict();
        let error = encode_stream_data(&dict, b"payload").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: ASCII85Encode is not supported: qpdf provides an ASCII85 decoder but no encoder"
        );
    }

    #[test]
    fn decode_stream_data_ascii85_empty() {
        let dict = ascii85_dict();
        let plaintext = b"";

        let encoded = ascii85_fixture_bytes(plaintext);
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_zero_block() {
        let dict = ascii85_dict();
        // A 4-byte all-zero block triggers the 'z' shorthand in the encoder
        let plaintext = [0u8; 8]; // two complete zero blocks → encoder emits "zz~>"

        let encoded = ascii85_fixture_bytes(&plaintext);
        // Verify the encoder actually used the 'z' shorthand
        assert!(
            encoded.contains(&b'z'),
            "encoder should emit 'z' for 4-byte zero block"
        );
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_short_final_group() {
        let dict = ascii85_dict();
        // Test all three short-final-group lengths: 1, 2, 3 bytes remainder
        for plaintext in [b"M".as_slice(), b"Ma", b"Man"] {
            let encoded = ascii85_fixture_bytes(plaintext);
            let decoded = decode_stream_data(&dict, &encoded).unwrap();
            assert_eq!(
                decoded,
                plaintext,
                "short final group round-trip failed for {} bytes",
                plaintext.len()
            );
        }
    }

    #[test]
    fn decode_stream_data_ascii85_rejects_invalid_byte() {
        let dict = ascii85_dict();
        // 'v' (0x76) is above the valid range '!'..'u' (0x21..=0x75)
        // Feed a hand-crafted stream: "9jqov~>" where 'v' is out-of-range
        let invalid_stream = b"9jqov~>";

        let error = decode_stream_data(&dict, invalid_stream).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: character out of range during base 85 decode"
        );
    }

    #[test]
    fn decode_stream_data_ascii85_uses_qpdf_overflow_and_whitespace_semantics() {
        assert_eq!(
            decode_stream_data(&ascii85_dict(), b"uuuuu").unwrap(),
            [0x08, 0x78, 0x0e, 0xc4]
        );
        assert_eq!(
            decode_stream_data(&ascii85_dict(), b"9j\x0bqo^").unwrap(),
            b"Man "
        );

        let error = decode_stream_data(&ascii85_dict(), b"!\0").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: character out of range during base 85 decode"
        );
    }

    // ----- RunLengthDecode filter integration tests -----

    fn run_length_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"RunLengthDecode".to_vec()));
        dict
    }

    #[test]
    fn encode_stream_data_run_length_qpdf_packets() {
        let dict = run_length_dict();

        assert_eq!(
            encode_stream_data(&dict, b"AA").unwrap(),
            [0xff, b'A', 0x80]
        );

        for length in [127, 128, 129] {
            let plaintext = vec![b'R'; length];
            let encoded = encode_stream_data(&dict, &plaintext).unwrap();
            assert_eq!(
                decode_stream_data(&dict, &encoded).unwrap(),
                plaintext,
                "length: {length}"
            );
        }
    }

    #[test]
    fn decode_stream_data_run_length_round_trip() {
        let dict = run_length_dict();
        let plaintext = b"Hello from RunLengthDecode filter!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_run_length_empty() {
        let dict = run_length_dict();
        let plaintext = b"";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_run_length_with_repeats() {
        let dict = run_length_dict();
        // Data with prominent repeat runs (triggers repeat-run encoding).
        let mut plaintext = vec![0x42u8; 100]; // 100 'B' bytes
        plaintext.extend(b"literal");
        plaintext.extend(vec![0xCCu8; 50]); // 50 0xCC bytes

        let encoded = encode_stream_data(&dict, &plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_run_length_uses_qpdf_partial_packet_and_eod_semantics() {
        assert_eq!(
            decode_stream_data(&run_length_dict(), &[0x05, b'A', b'B', b'C']).unwrap(),
            b"ABC"
        );
        assert_eq!(
            decode_stream_data(&run_length_dict(), &[0x80, 0x00, b'Z']).unwrap(),
            b"Z"
        );
    }

    #[test]
    fn ascii_and_run_length_filters_reject_decode_params_before_codec_work() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"ASCII85Decode", b"z"),
            (b"ASCIIHexDecode", b"41>"),
            (b"RunLengthDecode", &[0xff, b'A', 0x80]),
        ];

        for &(name, encoded) in cases {
            let mut dict = Dictionary::new();
            dict.insert("Filter", Object::Name(name.to_vec()));
            dict.insert("DecodeParms", Object::Dictionary(Dictionary::new()));

            let error = decode_stream_data(&dict, encoded).unwrap_err();

            assert_eq!(
                error.to_string(),
                format!(
                    "unsupported PDF feature: stream filter {} does not support supplied /DecodeParms",
                    String::from_utf8_lossy(name)
                ),
                "{name:?}"
            );
        }
    }

    // ----- Array filter chain round-trip tests (regression for flpdf-fh8) -----
    //
    // Per ISO 32000-1 §7.4.2, the /Filter array names the filters in the order
    // they must be applied to *decode* the stream. The encoder therefore has
    // to apply them in reverse so that `decode(encode(x))` round-trips for any
    // multi-element filter chain.

    fn array_filter_dict(filters: &[&[u8]]) -> Dictionary {
        let mut dict = Dictionary::new();
        let names: Vec<Object> = filters.iter().map(|f| Object::Name(f.to_vec())).collect();
        dict.insert("Filter", Object::Array(names));
        dict
    }

    fn native_encode_dictionary(dictionary: &Dictionary) -> ObjectHandle {
        ObjectHandle::dictionary(
            dictionary
                .iter()
                .map(|(key, value)| (key.to_vec(), handle_from_object(Some(value))))
                .collect(),
        )
    }

    fn comparable_encode(result: Result<Vec<u8>>) -> std::result::Result<Vec<u8>, String> {
        result.map_err(|error| error.to_string())
    }

    fn named_filter_dictionary(name: &[u8]) -> Dictionary {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter", Object::Name(name.to_vec()));
        dictionary
    }

    #[test]
    fn handle_encode_matches_dictionary_encode_for_the_full_filter_matrix() {
        let plain = b"ObjectHandle encode matrix: AAABBBCCCDDDEEE".to_vec();
        let mut rows: Vec<(String, Dictionary, Vec<u8>)> = Vec::new();

        rows.push((
            "missing /Filter".to_string(),
            Dictionary::new(),
            plain.clone(),
        ));

        for name in [
            b"FlateDecode".as_slice(),
            b"Fl",
            b"ASCII85Decode",
            b"A85",
            b"ASCIIHexDecode",
            b"AHx",
            b"RunLengthDecode",
            b"RL",
            b"LZWDecode",
            b"LZW",
            b"DCTDecode",
            b"DCT",
            b"CCITTFaxDecode",
            b"CCF",
            b"JBIG2Decode",
            b"JPXDecode",
            b"NoSuchDecode",
        ] {
            rows.push((
                String::from_utf8_lossy(name).into_owned(),
                named_filter_dictionary(name),
                plain.clone(),
            ));
        }

        rows.push((
            "ASCII85 then Flate chain".to_string(),
            array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]),
            plain.clone(),
        ));

        for predictor in 10..=15 {
            rows.push((
                format!("PNG predictor {predictor}"),
                png_predictor_dict(predictor, 4),
                sample_raw_4x2(),
            ));
        }

        let mut malformed_filter = Dictionary::new();
        malformed_filter.insert("Filter", Object::Integer(1));
        rows.push((
            "malformed /Filter".to_string(),
            malformed_filter,
            plain.clone(),
        ));

        let mut malformed_parms = named_filter_dictionary(b"FlateDecode");
        malformed_parms.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Null]),
        );
        rows.push((
            "misaligned /DecodeParms".to_string(),
            malformed_parms,
            plain.clone(),
        ));

        for (label, legacy, input) in rows {
            let native = native_encode_dictionary(&legacy);
            assert_eq!(
                comparable_encode(encode_stream_data(&legacy, &input)),
                comparable_encode(encode_stream_data_from_handle(&native, &input)),
                "encode paths diverged for {label}"
            );
        }
    }

    #[test]
    fn handle_encode_has_absolute_missing_run_length_and_chain_outputs() {
        let plain = b"AA";
        assert_eq!(
            encode_stream_data_from_handle(&ObjectHandle::dictionary(vec![]), plain).unwrap(),
            plain
        );

        let run_length = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"RunLengthDecode".to_vec()),
        )]);
        assert_eq!(
            encode_stream_data_from_handle(&run_length, plain).unwrap(),
            [0xff, b'A', 0x80]
        );

        let chain = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let native_chain = native_encode_dictionary(&chain);
        let payload = b"reverse-order chain payload";
        let error = encode_stream_data_from_handle(&native_chain, payload).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: ASCII85Encode is not supported: qpdf provides an ASCII85 decoder but no encoder"
        );
    }

    #[test]
    fn handle_encode_resolves_indirect_holder_filter_decode_parms_and_parameter_value() {
        let (columns, columns_resolver) = resolver_bearing_handle(ObjectValue::Integer(4));
        let (decode_params, decode_params_resolver) =
            resolver_bearing_handle(ObjectValue::Dictionary(
                [
                    (b"Predictor".to_vec(), ObjectHandle::integer(12)),
                    (b"Columns".to_vec(), columns.clone()),
                ]
                .into_iter()
                .collect(),
            ));
        let (filter, filter_resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"FlateDecode".to_vec()));
        let (stream_dictionary, dictionary_resolver) =
            resolver_bearing_handle(ObjectValue::Dictionary(
                [
                    (b"Filter".to_vec(), filter.clone()),
                    (b"DecodeParms".to_vec(), decode_params.clone()),
                ]
                .into_iter()
                .collect(),
            ));
        let _resolvers = (
            columns_resolver,
            decode_params_resolver,
            filter_resolver,
            dictionary_resolver,
        );

        assert!(!stream_dictionary.is_resolved());
        assert!(!filter.is_resolved());
        assert!(!decode_params.is_resolved());
        assert!(!columns.is_resolved());

        let raw = sample_raw_4x2();
        let actual = encode_stream_data_from_handle(&stream_dictionary, &raw).unwrap();
        let expected = encode_stream_data(&png_predictor_dict(12, 4), &raw).unwrap();

        assert!(stream_dictionary.is_resolved());
        assert!(filter.is_resolved());
        assert!(decode_params.is_resolved());
        assert!(columns.is_resolved());
        assert_eq!(actual, expected);
    }

    #[test]
    fn handle_encode_surfaces_a_dropped_document_from_the_dictionary_holder() {
        let (stream_dictionary, resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [(
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]
            .into_iter()
            .collect(),
        ));
        drop(resolver);

        let error = encode_stream_data_from_handle(&stream_dictionary, b"payload")
            .expect_err("a dropped document must not read as an empty filter chain");

        assert!(matches!(error, Error::Internal(_)));
        assert_eq!(error.to_string(), "object 20 0 belongs to a dropped PDF");
    }

    #[test]
    fn encode_stream_data_array_chain_rejects_ascii85_then_flate() {
        // Decoder order: ASCII85Decode, then FlateDecode.
        // Encoder must therefore apply FlateDecode first, then ASCII85Decode.
        let dict = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let plaintext = b"Round-trip me through ASCII85 over Flate, please!";

        let error = encode_stream_data(&dict, plaintext).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: ASCII85Encode is not supported: qpdf provides an ASCII85 decoder but no encoder"
        );
    }

    #[test]
    fn encode_stream_data_array_chain_round_trips_ascii_hex_then_flate() {
        let dict = array_filter_dict(&[b"ASCIIHexDecode", b"FlateDecode"]);
        let plaintext: Vec<u8> = (0u8..=200u8).collect();

        let encoded = encode_stream_data(&dict, &plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext);
    }

    #[test]
    fn encode_stream_data_array_chain_single_filter_matches_name_form() {
        // /Filter [/FlateDecode] should behave identically to /Filter /FlateDecode.
        let array_dict = array_filter_dict(&[b"FlateDecode"]);
        let name_dict = flate_dict();
        let plaintext = b"single-filter array form";

        let encoded_array = encode_stream_data(&array_dict, plaintext).unwrap();
        let encoded_name = encode_stream_data(&name_dict, plaintext).unwrap();

        assert_eq!(
            encoded_array, encoded_name,
            "Array form with one filter should produce the same bytes as the Name form"
        );
    }

    // ----- PNG predictor encode round-trip tests -----

    fn png_predictor_dict(predictor: i64, columns: i64) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(predictor));
        parms.insert("Columns", Object::Integer(columns));
        dict.insert("DecodeParms", Object::Dictionary(parms));
        dict
    }

    fn png_predictor_dict_rgb(predictor: i64, columns: i64) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(predictor));
        parms.insert("Columns", Object::Integer(columns));
        parms.insert("Colors", Object::Integer(3));
        parms.insert("BitsPerComponent", Object::Integer(8));
        dict.insert("DecodeParms", Object::Dictionary(parms));
        dict
    }

    /// Simple 2-row, 4-column grayscale raw data for predictor round-trip tests.
    fn sample_raw_4x2() -> Vec<u8> {
        vec![
            10, 20, 30, 40, // row 0
            50, 60, 70, 80, // row 1
        ]
    }

    #[test]
    fn encode_stream_data_png_predictor_10_round_trip() {
        let dict = png_predictor_dict(10, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_11_round_trip() {
        let dict = png_predictor_dict(11, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_12_round_trip() {
        let dict = png_predictor_dict(12, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_13_round_trip() {
        let dict = png_predictor_dict(13, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_14_round_trip() {
        let dict = png_predictor_dict(14, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_15_round_trip() {
        let dict = png_predictor_dict(15, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_handles_multi_row() {
        // row_bytes=8, rows=4 → 32 bytes total
        let dict = png_predictor_dict(12, 8);
        let raw: Vec<u8> = (0u8..32).collect();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_rgb_fixture_round_trip() {
        // Colors=3, BitsPerComponent=8, Columns=4 → row_bytes=12, rows=4 → 48 bytes
        let dict = png_predictor_dict_rgb(15, 4);
        let raw: Vec<u8> = (0u8..48).collect();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    /// Flate-compress `raw` with no predictor, for building predicted fixtures.
    fn flate_only(raw: &[u8]) -> Vec<u8> {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        encode_stream_data(&dict, raw).expect("flate encode")
    }

    /// qpdf's `Pl_PNGFilter` encoder always emits the Up row filter, so the
    /// predictor number selects only whether the predictor runs.
    #[test]
    fn every_png_predictor_encodes_up_rows() {
        let raw = sample_raw_4x2();
        let reference = encode_stream_data(&png_predictor_dict(12, 4), &raw).unwrap();

        for predictor in [10, 11, 13, 14, 15] {
            assert_eq!(
                encode_stream_data(&png_predictor_dict(predictor, 4), &raw).unwrap(),
                reference,
                "predictor {predictor}"
            );
        }
    }

    /// A predicted stream whose data stops mid-row decodes the partial row
    /// zero-padded instead of failing.
    #[test]
    fn png_predicted_stream_with_a_truncated_final_row_decodes_zero_padded() {
        let encoded = flate_only(&[0, 0x01, 0x02, 0x03, 0x04, 0, 0xff]);

        let decoded = decode_stream_data(&png_predictor_dict(12, 4), &encoded).unwrap();

        assert_eq!(
            decoded,
            vec![0x01, 0x02, 0x03, 0x04, 0xff, 0x00, 0x00, 0x00]
        );
    }

    /// A later stage with unusable predictor parameters is rejected even when
    /// an earlier stage would have tripped the output cap first.
    #[test]
    fn invalid_geometry_in_a_later_stage_is_reported_before_decoding() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(12));
        parms.insert("Columns", Object::Integer(4));
        parms.insert("BitsPerComponent", Object::Integer(3));
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"ASCII85Decode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Dictionary(parms)]),
        );

        // The ASCII85 stage alone would exceed this cap, so before the
        // preflight the Flate stage's geometry was never examined.
        let error = decode_stream_data_with_limits(
            &dict,
            b"9jqo^9jqo^~>",
            DecodeLimits {
                max_output: Some(1),
                ..DecodeLimits::default()
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: PNGFilter created with invalid bits_per_sample \
             not 1, 2, 4, 8, or 16"
        );
        assert!(!is_decode_output_limit_error(&error));
    }

    /// The preflight walks the chain in reverse, as qpdf's pipeline
    /// construction does, so the last unusable stage is reported first.
    #[test]
    fn preflight_reports_the_last_unusable_stage_first() {
        let mut first = Dictionary::new();
        first.insert("Predictor", Object::Integer(12));
        first.insert("Columns", Object::Integer(4));
        first.insert("Colors", Object::Integer(-1));
        let mut second = Dictionary::new();
        second.insert("Predictor", Object::Integer(12));
        second.insert("Columns", Object::Integer(4));
        second.insert("BitsPerComponent", Object::Integer(3));
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ]),
        );
        dict.insert(
            "DecodeParms",
            Object::Array(vec![Object::Dictionary(first), Object::Dictionary(second)]),
        );

        assert_eq!(
            decode_stream_data(&dict, b"").unwrap_err().to_string(),
            "unsupported PDF feature: PNGFilter created with invalid bits_per_sample \
             not 1, 2, 4, 8, or 16"
        );
    }

    /// An unrecognized row filter byte leaves the row unchanged.
    #[test]
    fn png_predicted_stream_ignores_an_unknown_row_filter_byte() {
        let encoded = flate_only(&[9, 0x01, 0x02, 0x03, 0x04]);

        let decoded = decode_stream_data(&png_predictor_dict(12, 4), &encoded).unwrap();

        assert_eq!(decoded, vec![0x01, 0x02, 0x03, 0x04]);
    }

    /// `/Columns` defaults to 1, so a predictor without it decodes one-byte rows.
    #[test]
    fn png_predictor_without_columns_uses_the_default_single_byte_row() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(12));
        dict.insert("DecodeParms", Object::Dictionary(parms));
        let encoded = flate_only(&[0, 0x41, 0, 0x42]);

        assert_eq!(decode_stream_data(&dict, &encoded).unwrap(), b"AB");
    }

    /// A non-null `/DecodeParms` value that is not an integer must still reach
    /// the filter as a non-integer once the shape-neutral `FilterSpec` has
    /// reduced it to `ParamValue::Name` or `ParamValue::Other`, so the stream
    /// stays unfilterable on both the decode and the encode side.
    #[test]
    fn non_null_non_integer_decode_params_values_remain_unfilterable() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Name(b"12".to_vec()));
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        let expected =
            "unsupported PDF feature: stream filter FlateDecode does not support supplied /DecodeParms";
        assert_eq!(
            decode_stream_data(&dict, b"not deflate data")
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(
            encode_stream_data(&dict, b"data").unwrap_err().to_string(),
            expected
        );
    }

    #[test]
    fn null_decode_params_values_are_omitted_before_decode_and_encode() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Null);
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        let encoded = flate_only(b"abc");
        assert_eq!(decode_stream_data(&dict, &encoded).unwrap(), b"abc");

        let reencoded = encode_stream_data(&dict, b"abc").unwrap();
        assert_eq!(decode_stream_data(&dict, &reencoded).unwrap(), b"abc");
    }

    #[test]
    fn encoding_rejects_null_decode_params_for_a_filter_that_does_not_enumerate_them() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Null);
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        assert_eq!(
            encode_stream_data(&dict, b"abc").unwrap_err().to_string(),
            "unsupported PDF feature: stream filter ASCIIHexDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn encoding_rejects_tiff_predictor_params_for_ascii85() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(2));
        parms.insert("Columns", Object::Integer(4));
        let mut dict = ascii85_dict();
        dict.insert("DecodeParms", Object::Dictionary(parms));

        assert_eq!(
            encode_stream_data(&dict, b"data").unwrap_err().to_string(),
            "unsupported PDF feature: stream filter ASCII85Decode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn encoding_rejects_a_predictor_outside_the_supported_set() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(9));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        assert_eq!(
            encode_stream_data(&dict, b"data").unwrap_err().to_string(),
            "unsupported PDF feature: stream filter FlateDecode does not support supplied /DecodeParms"
        );
    }

    #[test]
    fn encode_stream_data_tiff_predictor_2_round_trip() {
        let dict = png_predictor_dict(2, 4);
        let raw = sample_raw_4x2();

        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, raw);
    }

    #[test]
    fn encoding_rejects_negative_predictor_geometry() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(12));
        parms.insert("Columns", Object::Integer(4));
        parms.insert("Colors", Object::Integer(-1));
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        assert_eq!(
            encode_stream_data(&dict, b"data").unwrap_err().to_string(),
            "unsupported PDF feature: integer out of range converting -1 \
             from a 4-byte signed type to a 4-byte unsigned type"
        );
    }

    #[test]
    fn encoding_rejects_an_unsupported_bit_depth() {
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(12));
        parms.insert("Columns", Object::Integer(4));
        parms.insert("BitsPerComponent", Object::Integer(3));
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        assert_eq!(
            encode_stream_data(&dict, b"data").unwrap_err().to_string(),
            "unsupported PDF feature: PNGFilter created with invalid bits_per_sample \
             not 1, 2, 4, 8, or 16"
        );
    }

    /// The widest row a `/Columns` value can select before qpdf's 32-bit row
    /// arithmetic wraps to the rejected zero width. Two buffers of this size
    /// would be a gigabyte, so a stage that carries no data must not allocate.
    const WIDEST_ROW_COLUMNS: i64 = 0x1fff_ffff;

    #[test]
    fn png_predictor_encode_of_empty_input_skips_the_row_allocation() {
        let encoded = crate::stream_filter::encode_png_predictor(
            &[],
            u32::try_from(WIDEST_ROW_COLUMNS).unwrap(),
            1,
            8,
        )
        .unwrap();
        assert!(encoded.is_empty(), "empty input encodes to zero rows");
    }

    #[test]
    fn png_predictor_decode_of_empty_input_skips_the_row_allocation() {
        // Same guard on the untrusted decode path.
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(12));
        parms.insert("Columns", Object::Integer(WIDEST_ROW_COLUMNS));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        let encoded = encode_stream_data(&dict, &[]).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert!(decoded.is_empty(), "empty input decodes to zero rows");
    }

    #[test]
    fn encode_stream_data_png_predictor_empty_input_round_trips_without_oom() {
        // Mirrors the reported DoS: empty stream data plus a PNG predictor with a
        // huge /Columns. Encoding must succeed instead of aborting on an
        // enormous allocation. qpdf's Pl_Flate leaves its codec uninitialized
        // when it receives no bytes, so the encoded stream is also empty.
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(10));
        // `i32::MAX`, not `i64::MAX`: on 32-bit targets `usize::try_from(i64::MAX)`
        // fails, so /Columns would be rejected before the allocation path and the
        // test would panic for the wrong reason. This value still parses on both
        // widths and yields a large row width.
        parms.insert("Columns", Object::Integer(i32::MAX as i64));
        parms.insert("BitsPerComponent", Object::Integer(1));
        dict.insert("DecodeParms", Object::Dictionary(parms));

        let encoded = encode_stream_data(&dict, &[]).unwrap();
        assert!(encoded.is_empty(), "qpdf Pl_Flate emits no empty wrapper");

        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert!(decoded.is_empty(), "round-trips back to the empty input");
    }

    // ----- passthrough_codec_label tests (flpdf-9hc.7.4) -----

    #[test]
    fn passthrough_codec_label_recognizes_all_four_codecs() {
        assert_eq!(
            passthrough_codec_label(b"DCTDecode"),
            Some("DCTDecode"),
            "DCTDecode must be recognised"
        );
        assert_eq!(
            passthrough_codec_label(b"JBIG2Decode"),
            Some("JBIG2Decode"),
            "JBIG2Decode must be recognised"
        );
        assert_eq!(
            passthrough_codec_label(b"JPXDecode"),
            Some("JPXDecode"),
            "JPXDecode must be recognised"
        );
        assert_eq!(
            passthrough_codec_label(b"CCITTFaxDecode"),
            Some("CCITTFaxDecode"),
            "CCITTFaxDecode must be recognised"
        );
    }

    #[test]
    fn passthrough_codec_label_is_case_sensitive() {
        // PDF names are case-sensitive; lower-case variants must return None.
        assert_eq!(
            passthrough_codec_label(b"dctdecode"),
            None,
            "lowercase dctdecode must not match"
        );
        assert_eq!(
            passthrough_codec_label(b"jbig2decode"),
            None,
            "lowercase jbig2decode must not match"
        );
        assert_eq!(
            passthrough_codec_label(b"jpxdecode"),
            None,
            "lowercase jpxdecode must not match"
        );
        assert_eq!(
            passthrough_codec_label(b"ccittfaxdecode"),
            None,
            "lowercase ccittfaxdecode must not match"
        );
    }

    #[test]
    fn passthrough_codec_label_returns_none_for_unknown_filters() {
        assert_eq!(passthrough_codec_label(b"FlateDecode"), None);
        assert_eq!(passthrough_codec_label(b"LZWDecode"), None);
        assert_eq!(passthrough_codec_label(b"ASCII85Decode"), None);
        assert_eq!(passthrough_codec_label(b"ASCIIHexDecode"), None);
        assert_eq!(passthrough_codec_label(b"RunLengthDecode"), None);
        assert_eq!(passthrough_codec_label(b"UnknownFilter"), None);
        assert_eq!(passthrough_codec_label(b""), None);
    }

    // ----- is_decoded_filter tests -----

    #[test]
    fn is_decoded_filter_is_true_for_every_registered_decode_factory() {
        assert!(
            is_decoded_filter(b"DCTDecode"),
            "DCTDecode decodes via DctStreamFilter"
        );
        assert!(is_decoded_filter(b"FlateDecode"));
        assert!(is_decoded_filter(b"LZWDecode"));
        assert!(is_decoded_filter(b"ASCII85Decode"));
        assert!(is_decoded_filter(b"ASCIIHexDecode"));
        assert!(is_decoded_filter(b"RunLengthDecode"));
    }

    #[test]
    fn is_decoded_filter_is_false_for_crypt_despite_its_registered_factory() {
        // stream_filter_for(b"Crypt") returns Some(CryptStreamFilter), but
        // prepare_decode_filters always routes Crypt to the crypt provider
        // before consulting the registry — see the module doc's
        // "SF_Crypt::setDecodeParms... unreached" section. is_decoded_filter
        // must not claim Crypt is decodable through this path.
        assert!(!is_decoded_filter(b"Crypt"));
    }

    #[test]
    fn is_decoded_filter_is_false_for_the_remaining_undecoded_passthrough_codecs() {
        assert!(!is_decoded_filter(b"JBIG2Decode"));
        assert!(!is_decoded_filter(b"JPXDecode"));
        assert!(!is_decoded_filter(b"CCITTFaxDecode"));
    }

    #[test]
    fn is_decoded_filter_is_false_for_unknown_and_unnormalized_names() {
        assert!(!is_decoded_filter(b"UnknownFilter"));
        assert!(!is_decoded_filter(b""));
        // No normalization: the qpdf abbreviation does not match the
        // registry, which is keyed on the expanded name.
        assert!(!is_decoded_filter(b"DCT"));
    }

    // ----- flpdf-9hc.7.5: dispatch coverage tests -----

    /// Chain round-trip: Flate→ASCII85 encode, [/ASCII85Decode /FlateDecode] decode.
    /// This verifies that encode and decode correctly handle multi-filter chains
    /// (encode applies filters in reverse; decode applies in forward order).
    #[test]
    fn filter_chain_flate_ascii85_round_trip() {
        let dict = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let payload = b"chain round-trip: Flate + ASCII85 (flpdf-9hc.7.5)";

        let flate_encoded = encode_flate(payload).unwrap();
        let encoded = ascii85_fixture_bytes(&flate_encoded);
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, payload.as_slice());
    }

    /// Case-sensitivity: lowercase filter names must not match and must return Err.
    /// PDF names are case-sensitive per spec.
    #[test]
    fn filter_dispatch_is_case_sensitive() {
        // lowercase "flatedecode" is not a recognised filter
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"flatedecode".to_vec()));
        let result = decode_stream_data(&dict, b"anything");
        assert!(
            result.is_err(),
            "lowercase 'flatedecode' must not be accepted"
        );

        // passthrough_codec_label also rejects lowercase
        assert_eq!(
            passthrough_codec_label(b"dctdecode"),
            None,
            "passthrough_codec_label must be case-sensitive"
        );
        assert_eq!(passthrough_codec_label(b"jpxdecode"), None);

        // lowercase "dctdecode" in a stream dict must produce an unsupported Err
        let mut dict2 = Dictionary::new();
        dict2.insert("Filter", Object::Name(b"dctdecode".to_vec()));
        let result2 = decode_stream_data(&dict2, b"anything");
        assert!(
            result2.is_err(),
            "lowercase 'dctdecode' must not be accepted"
        );
        // message should NOT claim it is a passthrough codec; it is generic unsupported
        let msg2 = result2.unwrap_err().to_string();
        assert!(
            !msg2.contains("passthrough codec"),
            "lowercase filter should hit generic unsupported, not passthrough branch; got: {msg2}"
        );
    }

    /// A DCTDecode stage in a filter chain reaches the JPEG decoder after the
    /// preceding ASCII85 stage. The input must be valid ASCII85 data so that
    /// step 0 succeeds and step 1 (DCTDecode) is reached.
    #[test]
    fn dct_in_chain_rejects_malformed_jpeg_with_qpdf_diagnostic() {
        // Build a valid ASCII85-encoded payload so the first filter succeeds.
        let ascii85_encoded = ascii85_fixture_bytes(b"some binary jpeg payload");

        let dict = array_filter_dict(&[b"ASCII85Decode", b"DCTDecode"]);
        let result = decode_stream_data(&dict, &ascii85_encoded);

        assert!(
            result.is_err(),
            "malformed JPEG after ASCII85 must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("DCT decode"),
            "error must mention the DCT stage; got: {msg}"
        );
        assert!(
            msg.contains("Not a JPEG file: starts with 0x73 0x6f"),
            "error must preserve qpdf's JPEG diagnostic; got: {msg}"
        );
        assert!(
            !msg.contains("passthrough"),
            "DCT decoding must no longer report passthrough; got: {msg}"
        );
    }

    /// The whole-buffer DCT route's decode-bomb guard
    /// (`crates/flpdf/src/pipeline/dct.rs`, `PlDct::finish`) must classify
    /// the same way every other filter's `OutputBuffer::write` cap does —
    /// as [`is_decode_output_limit_error`] — all the way through
    /// `map_stage_error` -> `map_pipeline_error` -> `Error::Unsupported` at
    /// this public entry point. `check.rs`'s content-stream pass relies on
    /// that exact classification to downgrade a cap trip to a warning
    /// instead of a stream-encoding error; a stray identifier prefix on the
    /// message (this sentinel intentionally carries none, see
    /// [`DECODE_OUTPUT_LIMIT_PREFIX`]) would silently break that.
    #[test]
    fn dct_output_limit_is_classified_as_decode_output_limit_error() {
        let pixels = [0u8, 32, 64, 96, 128, 160, 192, 224, 255, 240, 120, 8];
        let jpeg = libjpeg_turbo_rs::compress(
            &pixels,
            2,
            2,
            libjpeg_turbo_rs::PixelFormat::Rgb,
            75,
            libjpeg_turbo_rs::Subsampling::S444,
        )
        .expect("test JPEG must encode");

        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"DCTDecode".to_vec()));

        let full = decode_stream_data(&dict, &jpeg).expect("unbounded DCT decode must succeed");
        assert_eq!(full.len(), 12); // 2x2 RGB, 3 bytes/pixel

        let err = decode_stream_data_with_limits(
            &dict,
            &jpeg,
            DecodeLimits {
                max_output: Some(full.len() - 1),
                ..DecodeLimits::default()
            },
        )
        .unwrap_err();
        assert!(
            is_decode_output_limit_error(&err),
            "DCT output-limit rejection must classify as the decode-bomb guard, not a generic stream error; got: {err}"
        );
    }

    /// LZWEncode is not supported: encode_stream_data with /LZWDecode filter must Err.
    #[test]
    fn lzw_encode_unsupported_returns_err() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));

        let result = encode_stream_data(&dict, b"some data");

        assert!(result.is_err(), "LZWEncode must not be supported");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LZWEncode"),
            "error must mention LZWEncode; got: {msg}"
        );
        assert!(
            msg.contains("FlateDecode only"),
            "error must mention FlateDecode only policy; got: {msg}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // LZWDecode malformed-code diagnostics
    //
    // qpdf's Pl_LZWDecoder distinguishes a code that is past the end of the
    // table from a code the table cannot resolve at all, and the public decode
    // path reports each verbatim.
    // ─────────────────────────────────────────────────────────────────────────

    fn decode_lzw(stream: &[u8]) -> Result<Vec<u8>> {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
        decode_stream_data(&dict, stream)
    }

    /// A table-backed code emitted while the table is still empty cannot be
    /// resolved. Both `[clear, 258]` and `[clear, 259]` reach that lookup,
    /// because a clear code suppresses the entry the next code would add.
    #[test]
    fn lzw_unresolvable_code_after_clear_reports_table_overflow() {
        for stream in [
            [0x80, 0x40, 0x80].as_slice(), // clear, 258
            [0x80, 0x40, 0xc0].as_slice(), // clear, 259
        ] {
            assert_eq!(
                decode_lzw(stream).unwrap_err().to_string(),
                "unsupported PDF feature: Pl_LZWDecoder::handleCode: table overflow",
                "{stream:?}"
            );
        }
    }

    /// A code more than one past the last allocated entry is rejected before
    /// the table is consulted.
    #[test]
    fn lzw_code_beyond_the_next_table_entry_is_rejected() {
        // clear, 'A', 259 - one entry exists, so index 1 is two past the end.
        assert_eq!(
            decode_lzw(&[0x80, 0x10, 0x60, 0x60])
                .unwrap_err()
                .to_string(),
            "unsupported PDF feature: LZWDecoder: bad code received"
        );
    }

    /// The decoder stops allocating at index 4096 rather than silently
    /// continuing with a frozen table.
    #[test]
    fn lzw_table_full_is_reported() {
        let mut codes = vec![256u32];
        codes.extend(std::iter::repeat_n(0x41u32, 3840));
        let stream = crate::pipeline::lzw::pack_codes(&codes, true);

        assert_eq!(
            decode_lzw(&stream).unwrap_err().to_string(),
            "unsupported PDF feature: LZWDecoder: table full"
        );
    }

    /// Trailing bits that do not complete a code are discarded without an
    /// error, and the bytes decoded before them are returned.
    #[test]
    fn lzw_truncated_trailing_bits_decode_without_an_error() {
        let stream = crate::pipeline::lzw::pack_codes(&[256, 0x41, 0x42], true);

        assert_eq!(decode_lzw(&stream).unwrap(), b"AB");
    }

    /// `/DecodeParms /EarlyChange` reaches the LZW codec through the reader.
    ///
    /// The one retained `/DecodeParms` key with no other absolute end-to-end
    /// test: `/Predictor`, `/Columns`, `/Colors` and `/BitsPerComponent` all
    /// change a decode here, and `/Name` is pinned by
    /// `crypt_stage_receives_the_name_parameter_a_provider_selects_on`, but
    /// `/EarlyChange`'s only entry-point rows are in the legacy-vs-native
    /// equivalence corpus, which is *relative* — dropping the key from
    /// `RETAINED_DECODE_PARAM_KEYS` moves both readers together and leaves
    /// that gate green.
    ///
    /// 300 literals cross LZW's first code-width transition, the only place
    /// `/EarlyChange` is observable. The stream is packed for `1` and declared
    /// as `0`, so the setting has to reach the codec for the two answers to
    /// differ at all.
    #[test]
    fn lzw_early_change_reaches_the_codec_from_decode_parms() {
        let codes: Vec<u32> = std::iter::once(256u32)
            .chain(std::iter::repeat_n(0x41u32, 300))
            .chain(std::iter::once(257))
            .collect();
        let plain = vec![b'A'; 300];
        let decode = |early_change: Option<i64>| {
            let mut dict = Dictionary::new();
            dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
            if let Some(early_change) = early_change {
                let mut parms = Dictionary::new();
                parms.insert("EarlyChange", Object::Integer(early_change));
                dict.insert("DecodeParms", Object::Dictionary(parms));
            }
            decode_stream_data(&dict, &crate::pipeline::lzw::pack_codes(&codes, true))
                .map_err(|error| error.to_string())
        };

        // The default and an explicit `1` match how the codes were packed.
        assert_eq!(decode(None).unwrap(), plain);
        assert_eq!(decode(Some(1)).unwrap(), plain);
        // `0` shifts the width transition by one code, so the decoder reads the
        // same bits as a different code sequence and runs off the table.
        assert_eq!(
            decode(Some(0)).unwrap_err(),
            "unsupported PDF feature: LZWDecoder: bad code received"
        );
    }

    // ----- Task 1: /Filter chain length cap (flpdf-hn1g.4) -----

    #[test]
    fn decode_rejects_overlong_filter_chain() {
        // 17 filters (> MAX_FILTER_CHAIN_LEN = 16) on the decode path is rejected
        // before any stage runs. The data is irrelevant; the cap trips first.
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"FlateDecode".to_vec()); 17]),
        );
        let err = decode_stream_data(&dict, b"anything");
        assert!(
            matches!(err, Err(Error::Unsupported(ref m)) if m.contains("filter chain length")),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_overlong_filter_chain_before_malformed_item() {
        let mut filters = vec![Object::Name(b"FlateDecode".to_vec()); 16];
        filters.push(Object::Integer(1));
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Array(filters));

        let error = decode_stream_data(&dict, b"anything").unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: filter chain length 17 exceeds maximum of 16"
        );
    }

    #[test]
    fn decode_stream_data_rejects_crypt_filter() {
        // The non-decrypting entry point cannot perform Crypt decryption, so a
        // `/Crypt` filter is rejected (decryption is only available through the
        // crypt-aware decode path). This exercises the default-crypt closure
        // `decode_stream_data_with_filters` installs.
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
        let err = decode_stream_data(&dict, b"data");
        assert!(
            matches!(err, Err(Error::Unsupported(ref m)) if m.contains("Crypt")),
            "got {err:?}"
        );
    }

    #[test]
    fn recovering_crypt_stage_emits_final_data_event() {
        let filter = Object::Name(b"Crypt".to_vec());
        let mut decrypt = |_: &DecodeParams, data: &[u8]| Ok(data.to_vec());

        let outcome = decode_stream_data_with_filters_and_crypt(
            Some(&filter),
            None,
            b"decrypted",
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .unwrap();

        assert_eq!(outcome.data, b"decrypted");
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Data(data)] if data == b"decrypted"
        ));
    }

    /// Plan decision D2: a Crypt provider selects its crypt filter from
    /// `/DecodeParms /Name`, so a name must survive the neutral spec as a name
    /// rather than collapsing into the `ParamValue::Other` stand-in.
    ///
    /// It is also what pins the Crypt arm to its stage's own parameters:
    /// substituting `DecodeParams::Absent` or a present-but-empty set for
    /// `stage.spec.decode_params` turns this assertion red. Those are the two
    /// constants the mutation actually proved — a constant carrying this same
    /// `/Name` would still pass, so this claim stops where the evidence does.
    #[test]
    fn crypt_stage_receives_the_name_parameter_a_provider_selects_on() {
        let filter = Object::Name(b"Crypt".to_vec());
        let mut parms = Dictionary::new();
        parms.insert("Name", Object::Name(b"Identity".to_vec()));
        let parms = Object::Dictionary(parms);
        let mut seen = Vec::new();
        let mut decrypt = |params: &DecodeParams, data: &[u8]| {
            seen = params.entries().to_vec();
            Ok(data.to_vec())
        };

        decode_stream_data_with_filters_and_crypt(
            Some(&filter),
            Some(&parms),
            b"payload",
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .unwrap();

        assert_eq!(
            seen,
            vec![(b"Name".to_vec(), ParamValue::Name(b"Identity".to_vec()))]
        );
    }

    /// The other half of the Crypt provider contract: an absent `/DecodeParms`
    /// reaches the provider as `DecodeParams::Absent`, not as a present-but-empty
    /// parameter set.
    #[test]
    fn crypt_stage_receives_neutral_decode_params() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
        let mut seen_absent = false;
        let result = decode_stream_data_with_filters_and_crypt(
            dict.get("Filter"),
            dict.get("DecodeParms"),
            b"payload",
            DecodeLimits::default(),
            DataEventMode::Suppress,
            &mut |params: &DecodeParams, data: &[u8]| {
                seen_absent = params.is_absent();
                Ok(data.to_vec())
            },
        );

        assert!(result.is_ok());
        assert!(seen_absent);
    }

    #[test]
    fn final_crypt_stage_preserves_pending_codec_events_and_cleanup_data() {
        let filter = Object::Array(vec![
            Object::Name(b"ASCIIHexDecode".to_vec()),
            Object::Name(b"Crypt".to_vec()),
        ]);
        let mut decrypt = |_: &DecodeParams, data: &[u8]| Ok(data.to_vec());

        let outcome = decode_stream_data_with_filters_and_crypt(
            Some(&filter),
            None,
            b"4G ",
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .expect("recover through the identity Crypt stage");

        assert_eq!(outcome.data, b"@");
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Error(Error::Unsupported(message)),
                StreamDecodeEvent::Data(data),
            ] if message == "character out of range during base Hex decode: G" && data == b"@"
        ));
    }

    #[test]
    fn leading_crypt_stage_decrypts_before_the_following_codec() {
        let filter = Object::Array(vec![
            Object::Name(b"Crypt".to_vec()),
            Object::Name(b"ASCIIHexDecode".to_vec()),
        ]);
        let mut decrypt = |_: &DecodeParams, data: &[u8]| {
            assert_eq!(data, b"encrypted");
            Ok(b"41>".to_vec())
        };

        let outcome = decode_stream_data_with_filters_and_crypt(
            Some(&filter),
            None,
            b"encrypted",
            DecodeLimits::default(),
            DataEventMode::Record,
            &mut decrypt,
        )
        .expect("decode after the leading Crypt stage");

        assert_eq!(outcome.data, b"A");
        assert!(matches!(
            &outcome.events[..],
            [StreamDecodeEvent::Data(data)] if data == b"A"
        ));
    }

    #[test]
    fn decode_accepts_max_length_filter_chain() {
        // Exactly MAX_FILTER_CHAIN_LEN (16) ASCIIHexDecode stages round-trips (each
        // stage is identity here: hex-encode applied 16 times, then this many decodes).
        // Build by encoding 16 times so the 16-deep decode chain reproduces the input.
        let original = b"hello";
        let mut data = original.to_vec();
        for _ in 0..16 {
            data = encode_stream_data(
                &{
                    let mut d = Dictionary::new();
                    d.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
                    d
                },
                &data,
            )
            .unwrap();
        }
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()); 16]),
        );
        let decoded = decode_stream_data(&dict, &data).unwrap();
        assert_eq!(decoded, original);
    }

    // ----- Task 2: opt-in DecodeLimits output cap (flpdf-hn1g.4) -----

    /// Pack a sequence of LZW codes as fixed 9-bit, MSB-first codewords (PDF
    /// LZWDecode initial width). flpdf has no LZW encoder, so tests synthesize
    /// minimal streams directly. Keeping every code 9 bits wide is valid only
    /// while the decoder's table stays below 511 entries (the first width-bump
    /// threshold under the default EarlyChange), i.e. fewer than ~253 literal
    /// codes — comfortably true for these fixtures.
    fn pack_lzw_9bit(codes: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;
        for &code in codes {
            buf = (buf << 9) | u32::from(code);
            bits += 9;
            while bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        if bits > 0 {
            out.push((buf << (8 - bits)) as u8);
        }
        out
    }

    #[test]
    fn flate_decode_honors_output_limit() {
        // 2000 'A' bytes compress small but decode large. A limit below 2000 is
        // rejected; a limit >= 2000 succeeds. Boundary: exactly 2000 succeeds.
        let raw = vec![b'A'; 2000];
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&dict, &raw).unwrap();

        // Under limit -> Unsupported.
        let err = decode_stream_data_with_limits(
            &dict,
            &encoded,
            DecodeLimits {
                max_output: Some(1999),
                ..DecodeLimits::default()
            },
        );
        assert!(
            matches!(err, Err(Error::Unsupported(ref m)) if m.contains("exceeds configured limit")),
            "got {err:?}"
        );
        // Exactly at limit -> Ok (boundary: take(limit+1) reads all 2000, len == limit).
        let ok = decode_stream_data_with_limits(
            &dict,
            &encoded,
            DecodeLimits {
                max_output: Some(2000),
                ..DecodeLimits::default()
            },
        )
        .unwrap();
        assert_eq!(ok.len(), 2000);
    }

    #[test]
    fn lzw_decode_honors_output_limit() {
        // Build a minimal LZW stream that decodes to 150 'A' bytes (code 65
        // repeated 150 times, then EOD=257). Each code emits one byte, so the
        // decoded length is deterministically 150. With every code 9 bits wide
        // (table never reaches 511 entries), the fixed-width packer is exact.
        let mut codes = vec![65u16; 150];
        codes.push(257); // EOD
        let lzw_bytes = pack_lzw_9bit(&codes);

        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
        let err = decode_stream_data_with_limits(
            &dict,
            &lzw_bytes,
            DecodeLimits {
                max_output: Some(100),
                ..DecodeLimits::default()
            },
        );
        assert!(
            matches!(err, Err(Error::Unsupported(ref m)) if m.contains("exceeds configured limit")),
            "got {err:?}"
        );
        // Unbounded still decodes fully.
        let full = decode_stream_data(&dict, &lzw_bytes).unwrap();
        assert_eq!(full, vec![b'A'; 150]);
        assert!(full.len() > 100);
    }

    #[test]
    fn decode_stream_data_is_unbounded_by_default() {
        // The legacy entry point keeps decoding arbitrarily large output (DecodeLimits
        // default = max_output None), guaranteeing backward compatibility.
        let raw = vec![b'Z'; 5000];
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        assert_eq!(decode_stream_data(&dict, &encoded).unwrap().len(), 5000);
        assert_eq!(DecodeLimits::default().max_output, None);
    }

    #[test]
    fn is_decode_output_limit_error_matches_only_the_limit_sentinel() {
        // The bounded-decode limit message is recognised as a limit trip...
        let limit_err = Error::Unsupported(format!("{DECODE_OUTPUT_LIMIT_PREFIX} 1024 bytes"));
        assert!(is_decode_output_limit_error(&limit_err));

        // ...but an unrelated Unsupported message (genuine corruption / unknown
        // codec) is not.
        let corrupt =
            Error::Unsupported("corrupt deflate stream: invalid distance code".to_string());
        assert!(!is_decode_output_limit_error(&corrupt));

        // ...and a non-Unsupported error never matches.
        let parse = Error::parse(0, "boom");
        assert!(!is_decode_output_limit_error(&parse));
    }

    /// Two PNG `None`-filtered rows of four data bytes each, plus the
    /// `/DecodeParms` that describe them.
    ///
    /// Without the predictor the same bytes decode to the ten raw bytes
    /// including each row's leading filter-type byte, so any reader that fails
    /// to pick `/DecodeParms` up off the dictionary produces a different
    /// length as well as different content.
    fn png_predicted_flate_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let raw = vec![0, 1, 2, 3, 4, 0, 5, 6, 7, 8];
        let encoded = encode_flate(&raw).unwrap();
        (encoded, raw, vec![1, 2, 3, 4, 5, 6, 7, 8])
    }

    /// The `/DecodeParms` describing [`png_predicted_flate_fixture`]'s rows.
    fn png_predicted_parms_handle() -> ObjectHandle {
        ObjectHandle::dictionary(vec![
            (b"Predictor".to_vec(), ObjectHandle::integer(12)),
            (b"Columns".to_vec(), ObjectHandle::integer(4)),
        ])
    }

    #[test]
    fn native_entry_point_decodes_a_flate_stream_from_a_handle() {
        let payload = b"canonical resolver payload";
        let encoded = crate::stream_filter::encode_flate(payload).unwrap();
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (
                b"Length".to_vec(),
                ObjectHandle::integer(encoded.len() as i64),
            ),
        ]);

        let decoded =
            decode_stream_data_from_handle(&dict, &encoded, DecodeLimits::default()).unwrap();

        assert_eq!(decoded, payload);
    }

    #[test]
    fn native_entry_point_reads_decode_parms_off_the_stream_dictionary() {
        let (encoded, undecoded, predicted) = png_predicted_flate_fixture();
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"DecodeParms".to_vec(), png_predicted_parms_handle()),
        ]);

        let decoded =
            decode_stream_data_from_handle(&dict, &encoded, DecodeLimits::default()).unwrap();

        // `QPDF_Stream::filterable` reads `/DecodeParms` at
        // `libqpdf/QPDF_Stream.cc:441`. Reading no key, or the wrong key,
        // leaves the predictor at 1 and yields `undecoded` instead.
        assert_eq!(decoded, predicted);
        assert_ne!(decoded, undecoded);
    }

    #[test]
    fn native_outcome_entry_point_carries_the_flate_warning_the_strict_form_raises() {
        // The same truncated-Flate-under-a-predictor input as
        // `recovering_final_flate_warning_precedes_predictor_finish_data`, so
        // the outcome carries a warning *and* a recovered data chunk.
        let mut encoded = encode_flate(b"\0A").unwrap();
        encoded.truncate(encoded.len() - 4);
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![
                    (b"Predictor".to_vec(), ObjectHandle::integer(12)),
                    (b"Columns".to_vec(), ObjectHandle::integer(2)),
                ]),
            ),
        ]);

        let outcome =
            decode_stream_data_recovering_from_handle(&dict, &encoded, DecodeLimits::default())
                .unwrap();

        assert_eq!(outcome.data, b"A\0");
        // The `Data` event is what separates this `Record`-mode entry point
        // from the `Suppress`-mode strict one below; the `Warning` is what a
        // bytes-only comparison would miss.
        assert!(matches!(
            &outcome.events[..],
            [
                StreamDecodeEvent::Warning(warning),
                StreamDecodeEvent::Data(data),
            ] if warning.message == "input stream is complete but output may still be valid"
                && warning.code == -5
                && data == b"A\0"
        ));

        // The strict form replays that same warning through
        // `replay_strict_decode_event`, so it must not answer `Ok`.
        let error =
            decode_stream_data_from_handle(&dict, &encoded, DecodeLimits::default()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream inflate: \
             input stream is complete but output may still be valid (zlib error -5)"
        );
    }

    #[test]
    fn native_entry_point_applies_the_caller_supplied_filter_chain_limit() {
        // `max_filter_chain` is the one `DecodeLimits` field the entry point
        // hands to the shape reader rather than to the shared engine, so it is
        // the field a wiring slip would silently drop. Passing a literal
        // `None` there leaves this the only red test.
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
        )]);
        let limits = DecodeLimits {
            max_output: None,
            max_filter_chain: Some(0),
        };

        let error = decode_stream_data_from_handle(&dict, b">", limits).unwrap_err();
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: filter chain length 1 exceeds maximum of 0"
        );

        // The same limit reaches the outcome-level entry point, and the
        // default limit admits this one-stage chain.
        assert!(decode_stream_data_recovering_from_handle(&dict, b">", limits).is_err());
        assert!(decode_stream_data_from_handle(&dict, b">", DecodeLimits::default()).is_ok());
    }

    #[test]
    fn native_entry_point_dereferences_the_stream_dictionary_holder_itself() {
        // `QPDF_Stream::filterable` reaches both keys through
        // `stream_dict.getKey` (`libqpdf/QPDF_Stream.cc:386`, `:441`), a
        // `QPDFObjectHandle` accessor that resolves the *holder* before
        // looking a key up. `try_get_key` is what reproduces that; the
        // non-resolving `get_key` would read this severed handle as "not a
        // dictionary", hand back a null `/Filter`, and answer `Ok` with the
        // bytes untouched — a broken document behind a plausible answer.
        //
        // `handle_reader_surfaces_a_dropped_document_from_every_child_position`
        // (`stream_filter.rs`) covers the *children*; the holder is reachable
        // only from this entry point, so it is pinned here.
        let (stream_dict, resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [(
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]
            .into_iter()
            .collect(),
        ));
        drop(resolver);

        let error =
            decode_stream_data_from_handle(&stream_dict, b"payload", DecodeLimits::default())
                .expect_err("a dropped document must not read as an empty filter chain");
        assert_eq!(error.to_string(), "object 20 0 belongs to a dropped PDF");
        assert!(matches!(error, Error::Internal(_)));

        // The outcome-level entry point shares the same helper, so it surfaces
        // the error rather than an empty event list.
        let outcome_error = decode_stream_data_recovering_from_handle(
            &stream_dict,
            b"payload",
            DecodeLimits::default(),
        )
        .unwrap_err();
        assert_eq!(
            outcome_error.to_string(),
            "object 20 0 belongs to a dropped PDF"
        );
    }

    #[test]
    fn native_entry_point_decodes_through_a_live_indirect_stream_dictionary() {
        // The shape `flpdf-25kg.3.5` will actually hand this entry point: an
        // unresolved indirect stream dictionary whose document is still alive.
        // `native_entry_point_dereferences_the_stream_dictionary_holder_itself`
        // above covers only the dropped-document half of `try_get_key`, and
        // every other handle test on this path passes a direct dictionary,
        // where `try_dereference` short-circuits on `Repr::Direct`.
        //
        // `_resolver` must stay bound to a name: `resolver_bearing_handle`'s
        // doc (`object_handle.rs`) records that binding it to `_` drops it at
        // once, which would silently turn this into a second dropped-document
        // test.
        let (encoded, undecoded, predicted) = png_predicted_flate_fixture();
        let (stream_dict, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [
                (
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
                (b"DecodeParms".to_vec(), png_predicted_parms_handle()),
            ]
            .into_iter()
            .collect(),
        ));

        let decoded =
            decode_stream_data_from_handle(&stream_dict, &encoded, DecodeLimits::default())
                .unwrap();

        // Both keys had to come back off the *resolved* dictionary: a missing
        // `/Filter` would leave the raw deflate bytes, and a missing
        // `/DecodeParms` would leave `undecoded`.
        assert_eq!(decoded, predicted);
        assert_ne!(decoded, undecoded);
        assert_ne!(decoded, encoded);
    }

    #[test]
    fn native_entry_point_routes_a_crypt_stage_to_the_shared_provider() {
        // Plan decision D2 of `flpdf-25kg.3.4` makes recognising a `Crypt`
        // stage and routing it to a provider the native path's job, and
        // `decode_stream_data_from_handle_with_mode` installs
        // `reject_crypt_stage` for that. Replacing that argument with an
        // identity closure leaves the whole suite green without this test:
        // `decode_stream_data_rejects_crypt_filter` above covers only the
        // legacy installation site.
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"Crypt".to_vec()),
        )]);

        let error =
            decode_stream_data_from_handle(&dict, b"payload", DecodeLimits::default()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: unsupported stream filter: Crypt"
        );
    }

    // ----- flpdf-25kg.3.4 Task 7: legacy-vs-native entry-point equivalence ---

    /// One corpus, run through the `&Dictionary` entry points and the
    /// `ObjectHandle` ones, compared row by row.
    ///
    /// # What is compared
    ///
    /// Each row goes through **both levels of both shapes**:
    ///
    /// - the recovering entry points ([`decode_stream_data_recovering_with_limits`]
    ///   against [`decode_stream_data_recovering_from_handle`]) — `Ok`/`Err`,
    ///   the decoded bytes, *and* the whole ordered event sequence: each
    ///   [`StreamDecodeEvent`]'s variant in emission order, a warning's message
    ///   and numeric code, an error's rendered text. [`StreamDecodeEvent`] is
    ///   not `PartialEq` (it carries an [`Error`]), so the sequence is
    ///   projected onto [`ComparableEvent`] rather than dropped, which is what
    ///   keeps this from degrading into a bytes-only comparison.
    /// - the strict entry points ([`decode_stream_data_with_limits`] against
    ///   [`decode_stream_data_from_handle`]) — the `Suppress`-mode wiring and
    ///   the replay that turns the first event into an `Err`.
    ///
    /// Both limits are swept: `max_filter_chain` over `[None, Some(16),
    /// Some(0)]`, because each shape reader hand-places two chain counts and
    /// only a cap makes their positions observable, and `max_output` over
    /// `[None, Some(1999), Some(2000)]`, the boundary
    /// [`flate_decode_honors_output_limit`] already pins around the
    /// 2000-byte row below. Both fields are `pub` on `DecodeLimits` and
    /// [`decode_stream_data_with_limits`] is `pub`, so every combination is
    /// reachable by an embedder.
    ///
    /// # Which rows carry event sequences
    ///
    /// A row compares an ordered event sequence only if its filter chain can be
    /// built at all. Many of [`shape_corpus`]'s rows are malformed on purpose
    /// and stop in the shape reader or in `prepare_decode_filters`, so they
    /// compare an `Err` string and nothing else. The multi-event orderings come
    /// mostly from [`payload_rows`] — several of which are the shapes
    /// `crates/flpdf/tests/stream_decode_recovery_public_api.rs` pins — plus the
    /// few shape rows that reach a codec.
    /// [`the_corpus_reaches_multi_event_warning_sequences`] asserts those
    /// orderings really are produced, so this module cannot hollow out into an
    /// error-string comparison unnoticed.
    ///
    /// # What this gate does and does not catch
    ///
    /// It catches divergence *between the two shapes* — a native reader or
    /// entry point that reads a key differently, orders events differently, or
    /// places a limit check differently from the legacy path, and equally a
    /// legacy-only drift in `decode_filter_specs_from_object`.
    ///
    /// It does **not** catch a drift below [`FilterSpec`]: both entry points
    /// funnel into the single [`decode_prepared_specs`], so a change there moves
    /// both sides together and this comparison stays green. Measured — reversing
    /// `events` at the end of [`decode_prepared_specs`] leaves
    /// [`legacy_and_native_entry_points_agree_on_every_corpus_row`] passing
    /// while reddening [`the_corpus_reaches_multi_event_warning_sequences`]
    /// together with the absolute event-order tests in this file (the
    /// `recovering_decode_*` family among them) and in
    /// `stream_decode_recovery_public_api.rs`. Those absolute pins are what
    /// covers the shared engine; this module is the relative gate.
    ///
    /// # This corpus is DIRECT-ONLY, and that limit is load-bearing
    ///
    /// Plan decision D1 of `flpdf-25kg.3.4` makes the two readers
    /// *deliberately* disagree on an indirect child: the legacy `as_*` reader
    /// classifies `Object::Reference` as [`ParamValue::Other`] (or as a
    /// non-name filter item), while the native `try_*` reader dereferences it —
    /// which the 2026-08-03 live-qpdf probe confirmed is what qpdf does.
    /// Asserting equivalence over an indirect row would assert the wrong thing,
    /// so [`handle_from_object`] panics on `Object::Reference` and this module
    /// must not add one. D1's own acceptance tests are the
    /// `handle_reader_dereferences_*` family in `stream_filter.rs`, plus
    /// [`native_entry_point_decodes_through_a_live_indirect_stream_dictionary`]
    /// at the entry-point level; do not "fix" the gap by widening the corpus
    /// here.
    ///
    /// Relatedly, an absent key and an explicitly null one are **the same
    /// native input**: [`handle_from_object`] maps both to a null handle, and
    /// `try_get_key` hands a missing key back as one too, exactly as qpdf's
    /// `getKey` does. The "absent /Filter" and "null /Filter" rows therefore
    /// agree trivially on the native side; it is the *legacy* side where they
    /// are distinct objects that both have to reach the same branch.
    ///
    /// # Proven to discriminate
    ///
    /// Two mutations of `decode_stream_data_from_handle_with_mode` were applied
    /// and reverted while writing this module:
    ///
    /// - dropping its `/DecodeParms` read (passing `&ObjectHandle::null()`
    ///   instead) reddens
    ///   [`legacy_and_native_entry_points_agree_on_every_corpus_row`] — the
    ///   shape half.
    /// - reversing the returned `outcome.events`, which leaves `outcome.data`
    ///   byte-identical, also reddens it, on the recovering comparison and with
    ///   both sides' `data` equal in the failure output — the ordering half, and
    ///   the direct evidence that this comparison is not bytes-only.
    mod equivalence {
        use super::*;
        use crate::stream_filter::tests::{handle_from_object, shape_corpus};

        /// [`StreamDecodeEvent`] reduced to something `PartialEq` can compare,
        /// keeping every field the two entry points must agree on.
        #[derive(Debug, PartialEq, Eq)]
        enum ComparableEvent {
            Data(Vec<u8>),
            Warning(String, i32),
            Error(String),
        }

        impl ComparableEvent {
            /// The variant name alone, for
            /// [`the_corpus_reaches_multi_event_warning_sequences`].
            fn variant(&self) -> &'static str {
                match self {
                    Self::Data(_) => "Data",
                    Self::Warning(..) => "Warning",
                    Self::Error(_) => "Error",
                }
            }
        }

        type ComparableOutcome = std::result::Result<(Vec<u8>, Vec<ComparableEvent>), String>;

        /// `Error` is not `PartialEq`, so render it; the two entry points must
        /// agree on `Ok`/`Err` *and* on the exact text.
        fn comparable_outcome(result: Result<StreamDecodeOutcome>) -> ComparableOutcome {
            result
                .map(|outcome| (outcome.data, comparable_events(outcome.events)))
                .map_err(|error| error.to_string())
        }

        fn comparable_events(events: Vec<StreamDecodeEvent>) -> Vec<ComparableEvent> {
            events
                .into_iter()
                .map(|event| match event {
                    StreamDecodeEvent::Data(data) => ComparableEvent::Data(data),
                    StreamDecodeEvent::Warning(warning) => {
                        ComparableEvent::Warning(warning.message, warning.code)
                    }
                    StreamDecodeEvent::Error(error) => ComparableEvent::Error(error.to_string()),
                })
                .collect()
        }

        fn comparable_bytes(result: Result<Vec<u8>>) -> std::result::Result<Vec<u8>, String> {
            result.map_err(|error| error.to_string())
        }

        struct Row {
            label: &'static str,
            filter: Option<Object>,
            decode_params: Option<Object>,
            stream_data: Vec<u8>,
        }

        impl Row {
            /// The legacy shape: keys are inserted only when the row has them,
            /// so `dict.get("Filter")` answers `None` for an absent one and
            /// `Some(&Object::Null)` for an explicitly null one.
            fn legacy_dictionary(&self) -> Dictionary {
                let mut dictionary = Dictionary::new();
                if let Some(filter) = &self.filter {
                    dictionary.insert("Filter", filter.clone());
                }
                if let Some(decode_params) = &self.decode_params {
                    dictionary.insert("DecodeParms", decode_params.clone());
                }
                dictionary
            }

            fn native_dictionary(&self) -> (ObjectHandle, Option<Rc<WarningRecorder>>) {
                let mut entries = Vec::new();
                if let Some(filter) = &self.filter {
                    entries.push((b"Filter".to_vec(), handle_from_object(Some(filter))));
                }
                let mut decode_params_resolver = None;
                if let Some(decode_params) = &self.decode_params {
                    let decode_params_handle =
                        if self.label == "present non-dictionary /DecodeParms" {
                            let (handle, resolver) = handle_resolving(ObjectValue::Integer(1));
                            decode_params_resolver = Some(resolver);
                            handle
                        } else {
                            handle_from_object(Some(decode_params))
                        };
                    entries.push((b"DecodeParms".to_vec(), decode_params_handle));
                }
                (ObjectHandle::dictionary(entries), decode_params_resolver)
            }
        }

        fn filter_name(name: &[u8]) -> Object {
            Object::Name(name.to_vec())
        }

        fn filter_array(names: &[&[u8]]) -> Object {
            Object::Array(names.iter().map(|name| filter_name(name)).collect())
        }

        fn parms_dictionary(entries: &[(&str, Object)]) -> Object {
            let mut dictionary = Dictionary::new();
            for (key, value) in entries {
                dictionary.insert(key, value.clone());
            }
            Object::Dictionary(dictionary)
        }

        /// ASCIIHex without the trailing `>`, so a byte appended after it is
        /// still read as a hex digit rather than sitting past EOD.
        /// `stream_decode_recovery_public_api.rs`'s own hex encoder emits no
        /// EOD for the same reason.
        fn asciihex_without_eod(data: &[u8]) -> Vec<u8> {
            let mut encoded = ascii_hex::encode(data);
            let eod = encoded.pop();
            assert_eq!(eod, Some(b'>'));
            encoded
        }

        /// Rows carrying real encoded payloads, so the corpus reaches the codec
        /// stack, the predictor, [`DecodeLimits::max_output`], and the
        /// warning/error ordering engine — none of which a malformed shape row
        /// ever gets to.
        fn payload_rows() -> Vec<Row> {
            let plain = b"equivalence corpus payload, compressible enough to matter".to_vec();
            let flate = encode_flate(&plain).unwrap();
            let mut truncated_flate = flate.clone();
            truncated_flate.truncate(truncated_flate.len() - 4);

            let (png_encoded, _, _) = png_predicted_flate_fixture();

            // 300 literals cross LZW's first code-width transition, which is
            // the only place `/EarlyChange` is observable: the same code
            // sequence packed for each setting decodes to the same plaintext
            // only when the decoder applies the matching transition point
            // (`pipeline::lzw::tests::early_code_change_shifts_the_width_transition_by_one_code`).
            let lzw_codes: Vec<u32> = std::iter::once(256u32)
                .chain(std::iter::repeat_n(0x41u32, 300))
                .chain(std::iter::once(257))
                .collect();
            let lzw_early = pack_codes(&lzw_codes, true);
            let lzw_late = pack_codes(&lzw_codes, false);

            // The boundary `flate_decode_honors_output_limit` pins: exactly
            // 2000 decoded bytes, so the `max_output` sweep straddles it.
            let big_flate = encode_flate(&vec![b'A'; 2000]).unwrap();

            let mut truncated_flate_then_bad_hex = asciihex_without_eod(&truncated_flate);
            truncated_flate_then_bad_hex.push(b'G');

            let mut flate_414 = encode_flate(b"414").unwrap();
            flate_414.truncate(flate_414.len() - 4);
            let mut flate_4g = encode_flate(b"4G ").unwrap();
            flate_4g.truncate(flate_4g.len() - 4);
            let mut flate_predictor_row = encode_flate(b"\0A").unwrap();
            flate_predictor_row.truncate(flate_predictor_row.len() - 4);

            // 17 ASCIIHex stages, encoded stage by stage: each round appends its
            // own `>` EOD, so the nesting unwinds one stage per decode.
            let mut one_stage = Dictionary::new();
            one_stage.insert("Filter", filter_name(b"ASCIIHexDecode"));
            let mut seventeen_stage_data = b"A".to_vec();
            for _ in 0..17 {
                seventeen_stage_data =
                    encode_stream_data(&one_stage, &seventeen_stage_data).unwrap();
            }

            vec![
                Row {
                    label: "flate payload",
                    filter: Some(filter_name(b"FlateDecode")),
                    decode_params: None,
                    stream_data: flate.clone(),
                },
                Row {
                    label: "flate under a PNG predictor",
                    filter: Some(filter_name(b"FlateDecode")),
                    decode_params: Some(parms_dictionary(&[
                        ("Predictor", Object::Integer(12)),
                        ("Columns", Object::Integer(4)),
                    ])),
                    stream_data: png_encoded,
                },
                Row {
                    label: "LZW at the default /EarlyChange",
                    filter: Some(filter_name(b"LZWDecode")),
                    decode_params: None,
                    stream_data: lzw_early.clone(),
                },
                Row {
                    label: "LZW with /EarlyChange 1",
                    filter: Some(filter_name(b"LZWDecode")),
                    decode_params: Some(parms_dictionary(&[("EarlyChange", Object::Integer(1))])),
                    stream_data: lzw_early.clone(),
                },
                Row {
                    label: "LZW with /EarlyChange 0",
                    filter: Some(filter_name(b"LZWDecode")),
                    decode_params: Some(parms_dictionary(&[("EarlyChange", Object::Integer(0))])),
                    stream_data: lzw_late,
                },
                // Packed for one setting and declared as the other, so
                // `/EarlyChange` has to actually reach the codec for this row
                // to differ from the two above.
                Row {
                    label: "LZW packed early but declaring /EarlyChange 0",
                    filter: Some(filter_name(b"LZWDecode")),
                    decode_params: Some(parms_dictionary(&[("EarlyChange", Object::Integer(0))])),
                    stream_data: lzw_early,
                },
                Row {
                    label: "ASCII85 payload",
                    filter: Some(filter_name(b"ASCII85Decode")),
                    decode_params: None,
                    stream_data: ascii85_fixture_bytes(&plain),
                },
                Row {
                    label: "ASCIIHex payload",
                    filter: Some(filter_name(b"ASCIIHexDecode")),
                    decode_params: None,
                    stream_data: ascii_hex::encode(&plain),
                },
                Row {
                    label: "RunLength payload",
                    filter: Some(filter_name(b"RunLengthDecode")),
                    decode_params: None,
                    stream_data: encode_run_length(&plain).unwrap(),
                },
                Row {
                    label: "chain of two: ASCIIHex then Flate",
                    filter: Some(filter_array(&[b"ASCIIHexDecode", b"FlateDecode"])),
                    decode_params: None,
                    stream_data: ascii_hex::encode(&flate),
                },
                Row {
                    label: "2000 decoded bytes, straddling the max_output sweep",
                    filter: Some(filter_name(b"FlateDecode")),
                    decode_params: None,
                    stream_data: big_flate,
                },
                Row {
                    label: "truncated flate",
                    filter: Some(filter_name(b"FlateDecode")),
                    decode_params: None,
                    stream_data: truncated_flate,
                },
                Row {
                    label: "corrupt flate",
                    filter: Some(filter_name(b"FlateDecode")),
                    decode_params: None,
                    stream_data: b"not a deflate stream at all".to_vec(),
                },
                // The remaining rows are the shapes
                // `stream_decode_recovery_public_api.rs` pins, which is where
                // the multi-event orderings come from.
                Row {
                    label: "AHx write error then a Flate finish warning",
                    filter: Some(filter_array(&[b"ASCIIHexDecode", b"FlateDecode"])),
                    decode_params: None,
                    stream_data: b"78G".to_vec(),
                },
                Row {
                    label: "AHx odd-nibble cleanup after a write error",
                    filter: Some(filter_name(b"AHx")),
                    decode_params: None,
                    stream_data: b"4G ".to_vec(),
                },
                Row {
                    label: "downstream data before an upstream write error",
                    filter: Some(filter_array(&[b"AHx", b"AHx"])),
                    decode_params: None,
                    stream_data: b"3431G".to_vec(),
                },
                Row {
                    label: "downstream cleanup after an upstream write error",
                    filter: Some(filter_array(&[b"AHx", b"AHx"])),
                    decode_params: None,
                    stream_data: b"343G".to_vec(),
                },
                Row {
                    label: "final data and warning after a prior write error",
                    filter: Some(filter_array(&[b"AHx", b"FlateDecode"])),
                    decode_params: None,
                    stream_data: truncated_flate_then_bad_hex,
                },
                Row {
                    label: "non-final warning between data and cleanup",
                    filter: Some(filter_array(&[b"FlateDecode", b"AHx"])),
                    decode_params: None,
                    stream_data: flate_414,
                },
                Row {
                    label: "final cleanup after a non-final warning and a write error",
                    filter: Some(filter_array(&[b"FlateDecode", b"AHx"])),
                    decode_params: None,
                    stream_data: flate_4g,
                },
                Row {
                    label: "final flate warning before predictor finish data",
                    filter: Some(filter_name(b"FlateDecode")),
                    decode_params: Some(parms_dictionary(&[
                        ("Predictor", Object::Integer(12)),
                        ("Columns", Object::Integer(2)),
                    ])),
                    stream_data: flate_predictor_row,
                },
                Row {
                    label: "17 ASCIIHex stages, decodable only with an unlimited chain",
                    filter: Some(Object::Array(vec![filter_name(b"ASCIIHexDecode"); 17])),
                    decode_params: None,
                    stream_data: seventeen_stage_data,
                },
            ]
        }

        /// [`shape_corpus`]'s rows — every `/Filter` + `/DecodeParms` shape
        /// `QPDF_Stream::filterable` distinguishes, including decision D4's
        /// null-valued `/DecodeParms` key — carried up from the unit-level
        /// reader gate to the entry points, plus [`payload_rows`].
        ///
        /// Each shape row decodes the same flate payload, so the shapes whose
        /// chain is buildable run a real codec instead of stopping at the
        /// reader.
        ///
        /// The D4 row ("null-valued /DecodeParms key (flpdf-h8mv)") pins the
        /// qpdf-compatible success path: qpdf's `QPDF_Dictionary::getKeys`
        /// skips null-valued entries, so the stream remains filterable. Both
        /// readers must preserve that behavior.
        fn corpus() -> Vec<Row> {
            let flate = encode_flate(b"shape corpus payload").unwrap();
            shape_corpus()
                .into_iter()
                .map(|(label, filter, decode_params)| Row {
                    label,
                    filter,
                    decode_params,
                    stream_data: flate.clone(),
                })
                .chain(payload_rows())
                .collect()
        }

        #[test]
        fn legacy_and_native_entry_points_agree_on_every_corpus_row() {
            for row in corpus() {
                let legacy = row.legacy_dictionary();
                let (native, _native_resolver) = row.native_dictionary();
                for max_filter_chain in [None, Some(16), Some(0)] {
                    for max_output in [None, Some(1999), Some(2000)] {
                        let limits = DecodeLimits {
                            max_output,
                            max_filter_chain,
                        };
                        let context = format!(
                            "row {:?} at max_filter_chain {max_filter_chain:?}, \
                             max_output {max_output:?}",
                            row.label
                        );

                        assert_eq!(
                            comparable_outcome(decode_stream_data_recovering_with_limits(
                                &legacy,
                                &row.stream_data,
                                limits,
                            )),
                            comparable_outcome(decode_stream_data_recovering_from_handle(
                                &native,
                                &row.stream_data,
                                limits,
                            )),
                            "recovering entry points diverged for {context}"
                        );
                        assert_eq!(
                            comparable_bytes(decode_stream_data_with_limits(
                                &legacy,
                                &row.stream_data,
                                limits,
                            )),
                            comparable_bytes(decode_stream_data_from_handle(
                                &native,
                                &row.stream_data,
                                limits,
                            )),
                            "strict entry points diverged for {context}"
                        );
                    }
                }
            }
        }

        /// The fourth input that collapses to `DecodeParams::Present(vec![])`.
        ///
        /// Three already did: an empty `/DecodeParms` dictionary, a present
        /// non-dictionary, and — since `9e1c4c66` — a non-consuming filter's
        /// view of a dictionary whose only entries are unresolved indirect
        /// values. `RETAINED_DECODE_PARAM_KEYS` adds a fourth: a dictionary
        /// holding nothing a consumer reads. That is only safe if no consumer
        /// can tell it from an empty one, so ask all of them — both decode
        /// entry points, strict and recovering, across the whole
        /// `max_output`/`max_filter_chain` sweep, plus the encode path — for a
        /// consuming filter, a non-consuming one, and a chain of both where
        /// the scalar `/DecodeParms` is replicated per stage.
        ///
        /// This is the empirical form of the claim `RETAINED_DECODE_PARAM_KEYS`
        /// makes in prose, and it is deliberately insensitive to retention
        /// itself: measured, deleting the `retains_decode_param_key` gate from
        /// `decode_params_from_object`, `decode_params_from_consuming_handle`,
        /// or `decode_params_from_entries` leaves it green. The dropped keys
        /// are genuinely inert. What this catches is the opposite change — a
        /// consumer that starts reading a key the constant does not name.
        #[test]
        fn a_dictionary_of_only_unread_keys_decodes_as_an_empty_one() {
            let plain = b"only-unread-keys payload".to_vec();
            let flate = encode_flate(&plain).unwrap();
            let unread = parms_dictionary(&[
                ("Unread", Object::Integer(1)),
                ("Type", Object::Name(b"CryptFilterDecodeParms".to_vec())),
                ("K", Object::Integer(-1)),
            ]);
            let empty = parms_dictionary(&[]);
            let cases: Vec<(&str, Object, Vec<u8>)> = vec![
                ("FlateDecode", filter_name(b"FlateDecode"), flate.clone()),
                (
                    "ASCIIHexDecode",
                    filter_name(b"ASCIIHexDecode"),
                    ascii_hex::encode(&plain),
                ),
                (
                    "chain replicating the scalar",
                    filter_array(&[b"ASCIIHexDecode", b"FlateDecode"]),
                    ascii_hex::encode(&flate),
                ),
            ];

            for (label, filter, stream_data) in cases {
                let row = |decode_params: &Object| Row {
                    label,
                    filter: Some(filter.clone()),
                    decode_params: Some(decode_params.clone()),
                    stream_data: stream_data.clone(),
                };
                let (unread_row, empty_row) = (row(&unread), row(&empty));
                let (unread_native, _unread_resolver) = unread_row.native_dictionary();
                let (empty_native, _empty_resolver) = empty_row.native_dictionary();

                for max_filter_chain in [None, Some(16), Some(0)] {
                    for max_output in [None, Some(1), Some(2000)] {
                        let limits = DecodeLimits {
                            max_output,
                            max_filter_chain,
                        };
                        let context = format!(
                            "{label} at max_filter_chain {max_filter_chain:?}, \
                             max_output {max_output:?}"
                        );

                        assert_eq!(
                            comparable_outcome(decode_stream_data_recovering_with_limits(
                                &unread_row.legacy_dictionary(),
                                &unread_row.stream_data,
                                limits,
                            )),
                            comparable_outcome(decode_stream_data_recovering_with_limits(
                                &empty_row.legacy_dictionary(),
                                &empty_row.stream_data,
                                limits,
                            )),
                            "legacy recovering told them apart for {context}"
                        );
                        assert_eq!(
                            comparable_bytes(decode_stream_data_with_limits(
                                &unread_row.legacy_dictionary(),
                                &unread_row.stream_data,
                                limits,
                            )),
                            comparable_bytes(decode_stream_data_with_limits(
                                &empty_row.legacy_dictionary(),
                                &empty_row.stream_data,
                                limits,
                            )),
                            "legacy strict told them apart for {context}"
                        );
                        assert_eq!(
                            comparable_outcome(decode_stream_data_recovering_from_handle(
                                &unread_native,
                                &unread_row.stream_data,
                                limits,
                            )),
                            comparable_outcome(decode_stream_data_recovering_from_handle(
                                &empty_native,
                                &empty_row.stream_data,
                                limits,
                            )),
                            "native recovering told them apart for {context}"
                        );
                        assert_eq!(
                            comparable_bytes(decode_stream_data_from_handle(
                                &unread_native,
                                &unread_row.stream_data,
                                limits,
                            )),
                            comparable_bytes(decode_stream_data_from_handle(
                                &empty_native,
                                &empty_row.stream_data,
                                limits,
                            )),
                            "native strict told them apart for {context}"
                        );
                    }
                }

                assert_eq!(
                    comparable_bytes(encode_stream_data(&unread_row.legacy_dictionary(), &plain)),
                    comparable_bytes(encode_stream_data(&empty_row.legacy_dictionary(), &plain)),
                    "the encode path told them apart for {label}"
                );
            }
        }

        #[test]
        fn the_corpus_reaches_multi_event_warning_sequences() {
            // Without this, the module's central claim would be unfalsifiable:
            // if every row failed in the shape reader, or the codec layer
            // stopped emitting warnings, the equivalence test above would keep
            // passing while comparing nothing but error strings. So pin the
            // orderings the corpus is supposed to produce, absolutely rather
            // than by agreement. Measured: reversing `events` at the end of
            // `decode_prepared_specs` reddens this test while leaving the
            // equivalence test green, because it moves both entry points
            // together — the one case the relative gate structurally cannot
            // see.
            //
            // Only the native entry point is asked; the equivalence test is
            // what makes the legacy side's answer the same.
            let outcomes: Vec<Vec<ComparableEvent>> = corpus()
                .iter()
                .filter_map(|row| {
                    let (native, _native_resolver) = row.native_dictionary();
                    decode_stream_data_recovering_from_handle(
                        &native,
                        &row.stream_data,
                        DecodeLimits::default(),
                    )
                    .ok()
                })
                .map(|outcome| comparable_events(outcome.events))
                .collect();
            let sequences: Vec<Vec<&'static str>> = outcomes
                .iter()
                .map(|events| events.iter().map(ComparableEvent::variant).collect())
                .collect();

            for expected in [
                vec!["Error", "Warning"],
                vec!["Error", "Data"],
                vec!["Data", "Error"],
                vec!["Data", "Error", "Warning"],
                vec!["Data", "Warning", "Data"],
                vec!["Warning", "Error", "Data"],
                vec!["Warning", "Data"],
            ] {
                assert!(
                    sequences.contains(&expected),
                    "no corpus row produced {expected:?}; observed {sequences:?}"
                );
            }

            // Those orderings are variant names, which throw the warning's own
            // text and zlib code away: decrementing `code` where
            // `decode_prepared_specs` builds the `StreamDecodeWarning` reddened
            // 10 absolute tests elsewhere while leaving both tests in this
            // module green. So pin the payload of the two diagnostics the
            // corpus leans on as well.
            let contains = |expected: ComparableEvent| {
                outcomes.iter().flatten().any(|event| *event == expected)
            };
            assert!(contains(ComparableEvent::Warning(
                "input stream is complete but output may still be valid".to_string(),
                -5,
            )));
            assert!(contains(ComparableEvent::Error(
                "unsupported PDF feature: character out of range during base Hex decode: G"
                    .to_string(),
            )));
        }
    }
}
