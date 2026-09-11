//! qpdf correspondence: QPDF_Stream filter-chain orchestration; QPDFStreamFilter dispatch, codec construction, and Pipeline execution are delegated to stream_filter.
use std::borrow::Cow;

use crate::object_handle::ObjectHandle;
use crate::stream_filter::{
    decode_filter_specs_from_handle, passthrough_codec_label as stream_passthrough_codec_label,
    stream_filter_for, undecodable_filter_error, FilterDecodePhase, FilterSpec,
    CRYPT_STAGE_UNSUPPORTED,
};
use crate::{Error, Result};

/// Maximum number of stages a `/Filter` chain may declare on the **decode**
/// path. Real PDFs use at most a few stages; this rejects only pathological
/// input where each stage re-expands the previous (multiplicative blow-up).
/// Unlike qpdf — which imposes no chain-length cap — flpdf rejects such chains
/// outright; this is an intentional divergence, not a compatibility target.
/// The encode path (writer output, not untrusted) is not capped.
const MAX_FILTER_CHAIN_LEN: usize = 16;

/// Return a human-readable codec label if `filter_name` is one of the four
/// image/binary codecs (`DCTDecode`, `JBIG2Decode`, `JPXDecode`,
/// `CCITTFaxDecode`) that the writer always emits verbatim rather than
/// re-encoding.
///
/// This is an **encode-side** classification: it does not indicate whether
/// [`ObjectHandle::get_stream_data`] can decode the codec. `DCTDecode` streams,
/// for example, are still reported here (the writer never re-encodes JPEG
/// data) even though the canonical stream pipe decodes them.
///
/// Comparison is **byte-exact** (PDF names are case-sensitive per spec).
/// Returns `None` for any other filter name.
pub fn passthrough_codec_label(filter_name: &[u8]) -> Option<&'static str> {
    stream_passthrough_codec_label(filter_name)
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
/// This opt-in boundary separates filter-chain interpretation/construction
/// from runtime codec failure. Unsupported filter shapes, names, and decode
/// parameters return an outer [`Error`]. A runtime error after successful
/// construction returns [`StreamDecodeOutcome`] with its partial bytes and
/// error populated. This applies [`DecodeLimits::default()`], including the
/// default 16-stage `/Filter` cap.
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
/// By default, output is unlimited while the `/Filter` chain is capped at 16
/// stages. Embedders processing untrusted input
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

/// The crypt provider every non-decrypting decode entry point installs.
///
/// Plan decision D2 of `flpdf-25kg.3.4` keeps decryption out of this layer, so
/// a `Crypt` stage is recognised during staging and then refused here. Shared
/// by the strict and recovering `ObjectHandle` entry points, and the message itself is
/// [`CRYPT_STAGE_UNSUPPORTED`] so that this provider and the registry-side
/// `CryptStreamFilter::pipe_decode_recovering` report one definition rather
/// than one literal per route.
fn reject_crypt_stage(_decode_params: &ObjectHandle, _data: &[u8]) -> Result<Vec<u8>> {
    Err(Error::Unsupported(CRYPT_STAGE_UNSUPPORTED.to_string()))
}

/// Decode a stream from its `ObjectHandle` stream dictionary while retaining
/// ordered recovery events — the `ObjectHandle`-native implementation behind
/// [`decode_stream_data_recovering_with_limits`].
///
/// # Errors
///
/// Returns an outer [`Error`] when the filter chain cannot be read or
/// constructed. Runtime codec failures instead remain ordered
/// [`StreamDecodeEvent::Error`] events alongside any recovered output.
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
type CryptProvider<'a> = &'a mut dyn FnMut(&ObjectHandle, &[u8]) -> Result<Vec<u8>>;

/// Run the staging, codec, and warning-ordering engine over already-read
/// filter specs.
///
/// Everything downstream of `FilterSpec` lives here in one copy. Production
/// callers enter through the recovering public wrapper; this keeps
/// filter-chain staging, predictor geometry, [`DecodeLimits::max_output`]
/// enforcement, and event ordering in one body without adding another
/// production stream-decoder boundary. Nothing in this body inspects a
/// `/Filter` or `/DecodeParms` object shape.
///
/// [`DecodeLimits::max_filter_chain`] is applied above this function, before
/// the handle reader copies its filter specs. The shared
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
                let mut next_pending_data_boundary = None;
                if let Some(boundary) = pending_data_boundary {
                    let prefix_data = &decoded[..boundary.0];
                    let prefix = decode_codec_prefix(&stage.spec, prefix_data, limits)?;
                    let input_end = if boundary.1 {
                        prefix.data.len()
                    } else {
                        prefix.cleanup_data_start
                    };
                    next_pending_data_boundary = Some(PendingDataBoundary(input_end, boundary.1));
                }
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
        if !adapter.set_decode_params(&spec.decode_params)? {
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
) -> Result<crate::stream_filter::FilterDecodeOutcome> {
    let filter_name = spec.normalized_name();
    let mut adapter =
        stream_filter_for(filter_name).expect("a prepared codec has a registered prefix decoder");
    // `debug_assert!` evaluates its expression only when debug assertions are
    // on, so applying the parameters *inside* the assertion silently skipped
    // the predictor in release builds and produced a different prefix length,
    // a different event boundary, and ultimately a different public error.
    let applied = adapter.set_decode_params(&spec.decode_params)?;
    debug_assert!(applied);
    adapter.set_tiff_memory_limit(limits.max_tiff_memory);
    Ok(adapter
        .pipe_decode_recovering(data, limits.max_output, &mut |_, _, _, _| Ok(()))
        .expect("preflighted codec prefix pipeline is infallible"))
}
