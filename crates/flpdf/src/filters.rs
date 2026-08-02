//! qpdf correspondence: QPDF_Stream filter-chain orchestration; QPDFStreamFilter dispatch, codec construction, and Pipeline execution are delegated to stream_filter.
use std::borrow::Cow;

use crate::ascii85;
use crate::ascii_hex;
use crate::pipeline::{PipelineError, PipelineResult};
#[cfg(test)]
use crate::stream_filter::expect_first_filter_input;
use crate::stream_filter::{
    decode_filter_specs_from_object, encode_flate, encode_run_length, stream_filter_for,
    DecodeParams, FilterDecodePhase, DECODE_OUTPUT_LIMIT_PREFIX,
};
use crate::{Dictionary, Error, Object, Result};

/// Maximum number of stages a `/Filter` chain may declare on the **decode**
/// path. Real PDFs use at most a few stages; this rejects only pathological
/// input where each stage re-expands the previous (multiplicative blow-up).
/// Unlike qpdf — which imposes no chain-length cap — flpdf rejects such chains
/// outright; this is an intentional divergence, not a compatibility target.
/// The encode path (writer output, not untrusted) is not capped.
const MAX_FILTER_CHAIN_LEN: usize = 16;

pub(crate) fn validate_filter_chain_len(filters: &[Object]) -> Result<()> {
    validate_filter_chain_count(filters.len(), Some(MAX_FILTER_CHAIN_LEN))
}

fn validate_filter_chain_count(count: usize, maximum: Option<usize>) -> Result<()> {
    if let Some(maximum) = maximum.filter(|maximum| count > *maximum) {
        return Err(Error::Unsupported(format!(
            "filter chain length {count} exceeds maximum of {maximum}"
        )));
    }
    Ok(())
}

/// Return a human-readable codec label if `filter_name` is an image/binary
/// passthrough codec that flpdf does not decode.
///
/// The four codecs (`DCTDecode`, `JBIG2Decode`, `JPXDecode`, `CCITTFaxDecode`)
/// are always emitted verbatim by the writer.  Callers (e.g. `show-stream`) can
/// use this function to distinguish "known-but-passthrough" filters from
/// genuinely unsupported ones.
///
/// Comparison is **byte-exact** (PDF names are case-sensitive per spec).
/// Returns `None` for any other filter name.
pub fn passthrough_codec_label(filter_name: &[u8]) -> Option<&'static str> {
    match filter_name {
        b"DCTDecode" => Some("DCTDecode"),
        b"JBIG2Decode" => Some("JBIG2Decode"),
        b"JPXDecode" => Some("JPXDecode"),
        b"CCITTFaxDecode" => Some("CCITTFaxDecode"),
        _ => None,
    }
}

/// Decode `stream_data` by applying the stream dictionary's `/Filter` chain,
/// honoring any `/DecodeParms`.
///
/// PNG predictors (`/Predictor 10` through `/Predictor 15`) are applied as part
/// of the chain. The TIFF predictor (`/Predictor 2`) is rejected.
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
/// - `/Predictor` is `2`, which selects the unsupported TIFF predictor.
/// - a predictor's row geometry is invalid: a negative `/Columns`, `/Colors`, or
///   `/BitsPerComponent`, a `/BitsPerComponent` outside `1`, `2`, `4`, `8`, and
///   `16`, or a row width that is zero.
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
/// decoding selects a filter per row from the row's own leading byte.
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
        &mut |_, _| {
            Err(Error::Unsupported(
                "unsupported stream filter: Crypt".to_string(),
            ))
        },
    )
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
    if let Some(Object::Array(filters)) = filter {
        validate_filter_chain_count(filters.len(), limits.max_filter_chain)?;
    }
    let specs = decode_filter_specs_from_object(filter, decode_params)?;
    validate_filter_chain_count(specs.len(), limits.max_filter_chain)?;
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
    spec: crate::stream_filter::FilterSpec,
    stage: PreparedStage,
}

fn prepare_decode_filters(
    specs: Vec<crate::stream_filter::FilterSpec>,
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
    spec: &crate::stream_filter::FilterSpec,
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

/// Report why a filter name has no decode route.
fn undecodable_filter_error(filter_name: &[u8]) -> Error {
    if let Some(label) = passthrough_codec_label(filter_name) {
        return Error::Unsupported(format!(
            "passthrough codec {label}: image/binary stream data is not decoded by flpdf (preserved verbatim)"
        ));
    }
    Error::Unsupported(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>")
    ))
}

fn encode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    let specs = decode_filter_specs_from_object(filter, decode_params)?;
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

/// Apply the PNG predictor selected by `/DecodeParms`, if any.
fn apply_encode_params(
    filter_name: &[u8],
    decode_params: &DecodeParams,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    match crate::stream_filter::png_encode_geometry(filter_name, decode_params)? {
        None => Ok(stream_data.to_vec()),
        Some((columns, colors, bits_per_component)) => crate::stream_filter::encode_png_predictor(
            stream_data,
            columns,
            colors,
            bits_per_component,
        ),
    }
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
        return Ok(ascii85::encode(stream_data));
    }

    if filter_name == b"ASCIIHexDecode" {
        return Ok(ascii_hex::encode(stream_data));
    }

    if filter_name == b"RunLengthDecode" {
        return encode_run_length(stream_data).map_err(|error| error.to_string());
    }

    // LZWEncode is not supported: flpdf writes stream compression as FlateDecode only
    // (decision flpdf-9hc.7.2; qpdf has no LZW encoder either).
    if filter_name == b"LZWDecode" {
        return Err(
            "LZWEncode is not supported: flpdf writes stream compression as FlateDecode only \
             (decision flpdf-9hc.7.2; qpdf has no LZW encoder either)"
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
    use crate::pipeline::lzw::pack_codes;
    use crate::stream_filter::ParamValue;

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
        let spec = crate::stream_filter::FilterSpec {
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
        let mut encoded = ascii85::encode(b"4   ");
        encoded.truncate(encoded.len() - 2);
        let cleanup = ascii85::encode(b"G");
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
        let mut encoded = ascii85::encode(b"4   ");
        encoded.truncate(encoded.len() - 2);
        let cleanup = ascii85::encode(b"G");
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

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_empty() {
        let dict = ascii85_dict();
        let plaintext = b"";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_zero_block() {
        let dict = ascii85_dict();
        // A 4-byte all-zero block triggers the 'z' shorthand in the encoder
        let plaintext = [0u8; 8]; // two complete zero blocks → encoder emits "zz~>"

        let encoded = encode_stream_data(&dict, &plaintext).unwrap();
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
            let encoded = encode_stream_data(&dict, plaintext).unwrap();
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

    #[test]
    fn encode_stream_data_array_chain_round_trips_ascii85_then_flate() {
        // Decoder order: ASCII85Decode, then FlateDecode.
        // Encoder must therefore apply FlateDecode first, then ASCII85Decode.
        let dict = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let plaintext = b"Round-trip me through ASCII85 over Flate, please!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
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

    /// A `/DecodeParms` value that is not an integer must still reach the
    /// filter as a non-integer once the shape-neutral `FilterSpec` has reduced
    /// it to `ParamValue::Name` or `ParamValue::Other`, so the stream stays
    /// unfilterable on both the decode and the encode side.
    #[test]
    fn non_integer_decode_params_values_remain_unfilterable() {
        for value in [Object::Name(b"12".to_vec()), Object::Null] {
            let mut parms = Dictionary::new();
            parms.insert("Predictor", value.clone());
            let mut dict = Dictionary::new();
            dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            dict.insert("DecodeParms", Object::Dictionary(parms));

            let expected =
                "unsupported PDF feature: stream filter FlateDecode does not support supplied /DecodeParms";
            assert_eq!(
                decode_stream_data(&dict, b"not deflate data")
                    .unwrap_err()
                    .to_string(),
                expected,
                "decode {value:?}"
            );
            assert_eq!(
                encode_stream_data(&dict, b"data").unwrap_err().to_string(),
                expected,
                "encode {value:?}"
            );
        }
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
    fn encoding_reports_the_tiff_predictor_restriction() {
        assert_eq!(
            encode_stream_data(&png_predictor_dict(2, 4), b"data")
                .unwrap_err()
                .to_string(),
            "unsupported PDF feature: /DecodeParms /Predictor 2 is not supported for this stream type"
        );
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

    // ----- flpdf-9hc.7.5: dispatch coverage tests -----

    /// Chain round-trip: Flate→ASCII85 encode, [/ASCII85Decode /FlateDecode] decode.
    /// This verifies that encode and decode correctly handle multi-filter chains
    /// (encode applies filters in reverse; decode applies in forward order).
    #[test]
    fn filter_chain_flate_ascii85_round_trip() {
        let dict = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let payload = b"chain round-trip: Flate + ASCII85 (flpdf-9hc.7.5)";

        let encoded = encode_stream_data(&dict, payload).unwrap();
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

    /// passthrough-in-chain: /Filter [/ASCII85Decode /DCTDecode] decode must return Err
    /// because DCTDecode is a passthrough codec that flpdf does not decode.
    /// The input must be valid ASCII85 data so that step 0 succeeds and
    /// step 1 (DCTDecode) is reached.
    #[test]
    fn passthrough_in_chain_returns_err_with_passthrough_message() {
        // Build a valid ASCII85-encoded payload so the first filter succeeds.
        let ascii85_encoded = ascii85::encode(b"some binary jpeg payload");

        let dict = array_filter_dict(&[b"ASCII85Decode", b"DCTDecode"]);
        let result = decode_stream_data(&dict, &ascii85_encoded);

        assert!(
            result.is_err(),
            "chain containing DCTDecode must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("DCTDecode"),
            "error must mention DCTDecode; got: {msg}"
        );
        assert!(
            msg.contains("passthrough"),
            "error must indicate passthrough intent; got: {msg}"
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
}
