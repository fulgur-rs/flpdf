//! qpdf correspondence: QPDF_Stream filter-chain orchestration; QPDFStreamFilter dispatch, codec construction, and Pipeline execution are delegated to stream_filter.
use std::borrow::Cow;

use crate::object_handle::ObjectHandle;
use crate::pipeline::{PipelineError, PipelineResult};
use crate::stream_filter::{
    decode_filter_specs_from_handle, encode_flate, encode_run_length,
    is_decoded_filter as stream_is_decoded_filter,
    passthrough_codec_label as stream_passthrough_codec_label, stream_filter_for,
    undecodable_filter_error, DecodeParams, FilterDecodePhase, FilterSpec, CRYPT_STAGE_UNSUPPORTED,
};
use crate::{Error, Result};

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
pub(crate) fn stream_filter_capabilities(
    stream_dict: &ObjectHandle,
) -> Option<StreamFilterCapabilities> {
    let filter = stream_dict.try_get_key(b"/Filter").ok()?;
    let decode_params = stream_dict.try_get_key(b"/DecodeParms").ok()?;
    let specs =
        decode_filter_specs_from_handle(&filter, &decode_params, Some(MAX_FILTER_CHAIN_LEN))
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
pub fn decode_stream_data(stream_dict: &ObjectHandle, stream_data: &[u8]) -> Result<Vec<u8>> {
    decode_stream_data_with_limits_and_warnings(
        stream_dict,
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
pub(crate) enum DataEventMode {
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
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_recovering_with_limits(stream_dict, stream_data, DecodeLimits::default())
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
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<StreamDecodeOutcome> {
    decode_stream_data_recovering_from_handle(stream_dict, stream_data, limits)
}

/// Opt-in limits applied while decoding a stream's filter chain.
///
/// By default, output is unlimited, matching [`decode_stream_data`], while the
/// `/Filter` chain is capped at 16 stages. Embedders processing untrusted input
/// can set [`max_output`](Self::max_output) to bound the decoded size of each
/// `FlateDecode`, `LZWDecode`, `ASCII85Decode`, `ASCIIHexDecode`, or
/// `RunLengthDecode` stage, trading completeness for a per-stage bound. It is
/// not a ceiling on the total work or cumulative output across a filter chain.
/// [`max_tiff_memory`](Self::max_tiff_memory) is a separate optional qpdf-head
/// hardening budget for TIFF predictor row geometry; `None` and `Some(0)` are
/// unlimited, matching qpdf's zero-valued global limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum decoded byte count permitted out of any single supported filter
    /// stage, counted after that stage's predictor if it has one. `None`
    /// (default) is unlimited.
    pub max_output: Option<usize>,
    /// Maximum TIFF predictor row-memory budget in bytes. qpdf's hardening
    /// rejects a row when its wide `bytes_per_row` exceeds half this value,
    /// before partial-row padding or predictor state allocation. `None` and
    /// `Some(0)` (default) leave the pinned qpdf 11.9.0 behavior unlimited.
    pub max_tiff_memory: Option<usize>,
    /// Maximum `/Filter` stages accepted before individual filter items are
    /// validated. `None` disables this count limit.
    pub max_filter_chain: Option<usize>,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_output: None,
            max_tiff_memory: None,
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
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
    limits: DecodeLimits,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    let outcome = decode_stream_data_from_handle_with_mode(
        stream_dict,
        stream_data,
        limits,
        DataEventMode::Suppress,
    )?;
    replay_strict_decode_outcome(outcome, warn)
}

/// Collapse a recovered outcome into the strict `getStreamData` shape: the
/// first replayed error wins, otherwise the decoded bytes.
///
/// Shared by the strict public handle path and the
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
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
    limits: DecodeLimits,
) -> Result<Vec<u8>> {
    decode_stream_data_with_limits_and_warnings(
        stream_dict,
        stream_data,
        limits,
        &mut reject_decode_warning,
    )
}

/// Encode `stream_data` by applying the stream dictionary's write-supported
/// `/Filter` chain.
///
/// This is not a complete inverse of [`decode_stream_data`]. qpdf exposes
/// ASCII85 and ASCIIHex decoding but no corresponding encoders, so chains that
/// contain `/ASCII85Decode` or `/ASCIIHexDecode` return [`Error::Unsupported`].
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
/// - a filter is decode-only on the encode path, including `/ASCII85Decode`,
///   `/ASCIIHexDecode`, and `LZWDecode`.
pub fn encode_stream_data(stream_dict: &ObjectHandle, stream_data: &[u8]) -> Result<Vec<u8>> {
    encode_stream_data_from_handle(stream_dict, stream_data)
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
/// including the explicit unsupported results for `/ASCII85Decode` and
/// `/ASCIIHexDecode`, plus
/// [`Error::Internal`] if an indirect holder or child still needs a document
/// resolver after its document has been dropped.
pub(crate) fn encode_stream_data_from_handle(
    stream_dict: &ObjectHandle,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    let filter = stream_dict.try_get_key(b"/Filter")?;
    let decode_params = stream_dict.try_get_key(b"/DecodeParms")?;
    let specs = decode_filter_specs_from_handle(&filter, &decode_params, None)?;
    encode_stream_data_from_specs(specs, stream_data)
}

/// The crypt provider every non-decrypting decode entry point installs.
///
/// Plan decision D2 of `flpdf-25kg.3.4` keeps decryption out of this layer, so
/// a `Crypt` stage is recognised during staging and then refused here. Shared
/// by the strict and recovering `ObjectHandle` entry points, and the message itself is
/// [`CRYPT_STAGE_UNSUPPORTED`] so that this provider and the registry-side
/// `CryptStreamFilter::pipe_decode_recovering` report one definition rather
/// than one literal per route.
fn reject_crypt_stage(_decode_params: &DecodeParams, _data: &[u8]) -> Result<Vec<u8>> {
    Err(Error::Unsupported(CRYPT_STAGE_UNSUPPORTED.to_string()))
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
/// them out of the handle, because the resolver already retained them from
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
/// same private helper and then applies the strict public path's replay, so
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
/// function both the production handle reader and the test-only materialized
/// fixture reader call. Plan decision D2 of `flpdf-25kg.3.4`
/// keeps it an explicit parameter instead of a document hookup.
type CryptProvider<'a> = &'a mut dyn FnMut(&DecodeParams, &[u8]) -> Result<Vec<u8>>;

/// Run the staging, codec, and warning-ordering engine over already-read
/// filter specs.
///
/// Everything downstream of `FilterSpec` lives here in one copy. Production
/// callers enter through [`decode_stream_data_from_handle`]; the materialized
/// reader is compiled only for the in-module equivalence fixture. This keeps
/// filter-chain staging, predictor geometry, [`DecodeLimits::max_output`]
/// enforcement, and event ordering in one body without retaining a legacy
/// production boundary. Nothing in this body inspects a `/Filter` or
/// `/DecodeParms` object shape.
///
/// [`DecodeLimits::max_filter_chain`] is applied above this function, before
/// either reader snapshots its filter specs. The shared
/// `validate_filter_chain_count` keeps the error text identical; the
/// test-only equivalence corpus pins the placement against the handle reader.
fn decode_prepared_specs(
    specs: Vec<FilterSpec>,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
    decrypt_crypt: CryptProvider<'_>,
) -> Result<StreamDecodeOutcome> {
    let prepared = prepare_decode_filters(specs, limits.max_tiff_memory)?;
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

fn prepare_decode_filters(
    specs: Vec<FilterSpec>,
    max_tiff_memory: Option<usize>,
) -> Result<Vec<PreparedDecodeFilter>> {
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
        adapter.set_tiff_memory_limit(max_tiff_memory);

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
    adapter.set_tiff_memory_limit(limits.max_tiff_memory);
    adapter
        .pipe_decode_recovering(data, limits.max_output, &mut |_, _, _, _| Ok(()))
        .expect("preflighted codec prefix pipeline is infallible")
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
        return Err(
            "ASCIIHexEncode is not supported: qpdf provides an ASCIIHex decoder but no encoder"
                .to_string(),
        );
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
