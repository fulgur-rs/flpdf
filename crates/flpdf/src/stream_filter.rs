//! qpdf correspondence: `QPDFStreamFilter.cc` and `QPDF_Stream.cc` filter names, full `/DecodeParms` handles, and reverse decode-pipeline construction (`libqpdf/QPDF_Stream.cc:380-482`).
//!
//! Each [`FilterSpec`] retains the native [`ObjectHandle`] supplied by the
//! stream dictionary. This mirrors qpdf's shared `QPDFObjectHandle` across
//! the filter chain: setters inspect the live handle in qpdf order, rather
//! than through a reduced snapshot or copied parameter-value enum. In
//! particular, the Flate/LZW and Crypt setters below perform their own
//! qpdf-shaped key walks and validation, while filters inheriting the base
//! setter only test whether the handle is null.
//!
//! Crypt decryption remains owned by
//! `reader::resolver::inspect_stream_encryption` and
//! `reader::resolver::pipe_stream_data_from_input`; this module only preserves
//! qpdf's filterability and no-stage pipeline contract. The `filters` layer
//! supplies the non-decrypting rejection provider at that boundary.

use crate::object_handle::ObjectHandle;
use crate::pipeline::ascii85_decoder::Ascii85Decoder;
use crate::pipeline::ascii_hex::AsciiHexDecoder;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::dct::PlDct;
use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
use crate::pipeline::lzw::LzwDecoder;
use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
use crate::pipeline::run_length::{RunLength, RunLengthAction};
use crate::pipeline::tiff_predictor::{TiffPredictor, TiffPredictorAction};
use crate::pipeline::{Pipeline, PipelineError, PipelineRef, PipelineResult};
use crate::{Error, Result};
use std::cell::Cell;
use std::rc::Rc;

pub(crate) const DECODE_OUTPUT_LIMIT_PREFIX: &str = "decoded output exceeds configured limit of";

/// The message a refused `Crypt` stage reports, wherever the refusal happens.
///
/// Two routes can produce it: `filters::reject_crypt_stage`, the crypt provider
/// every non-decrypting decode entry point installs, and
/// [`CryptStreamFilter`]'s `pipe_decode_recovering`, the registry-side route
/// nothing reaches today. One definition rather than one literal per route is
/// what makes the public error genuinely fixed if a `Crypt` stage is ever
/// routed through the registry — two literals could only ever happen to agree.
pub(crate) const CRYPT_STAGE_UNSUPPORTED: &str = "unsupported stream filter: Crypt";

type FilterWarningCallback = Box<dyn FnMut(&str, i32) -> PipelineResult<()> + 'static>;

#[derive(Debug)]
pub(crate) struct FilterSpec {
    pub(crate) name: Vec<u8>,
    pub(crate) decode_params: ObjectHandle,
}

impl FilterSpec {
    pub(crate) fn normalized_name(&self) -> &[u8] {
        normalize_filter_name(&self.name)
    }
}

pub(crate) fn normalize_filter_name(name: &[u8]) -> &[u8] {
    match name {
        b"Fl" => b"FlateDecode",
        b"LZW" => b"LZWDecode",
        b"A85" => b"ASCII85Decode",
        b"AHx" => b"ASCIIHexDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        name => name,
    }
}

/// Return a human-readable codec label if `filter_name` is one of the four
/// image/binary codecs (`DCTDecode`, `JBIG2Decode`, `JPXDecode`,
/// `CCITTFaxDecode`) that the writer always emits verbatim rather than
/// re-encoding.
///
/// This is an **encode-side** classification, independent of whether flpdf's
/// decode path can currently decode the codec — see [`is_decoded_filter`] for
/// that question. Keeping this classification beside the filter registry lets
/// the qpdf-shaped factory check use the same diagnostic that the later
/// decode stage would have produced for a codec with no decode factory at
/// all.
pub(crate) fn passthrough_codec_label(filter_name: &[u8]) -> Option<&'static str> {
    match filter_name {
        b"DCTDecode" => Some("DCTDecode"),
        b"JBIG2Decode" => Some("JBIG2Decode"),
        b"JPXDecode" => Some("JPXDecode"),
        b"CCITTFaxDecode" => Some("CCITTFaxDecode"),
        _ => None,
    }
}

/// Return whether flpdf's decode path can actually decode `filter_name`.
///
/// [`stream_filter_for`] registers a factory for `Crypt` too, but
/// `filters::prepare_decode_filters` always routes a `Crypt` spec to the
/// installed crypt provider before consulting the registry, so `Crypt` is
/// excluded here to keep this predicate honest about what a caller like
/// `show-stream` will actually observe.
pub(crate) fn is_decoded_filter(filter_name: &[u8]) -> bool {
    filter_name != b"Crypt" && stream_filter_for(filter_name).is_some()
}

/// Report why a filter name has no decode factory.
pub(crate) fn undecodable_filter_error(filter_name: &[u8]) -> Error {
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

/// Validate `/Filter` names at the same stage as qpdf's `filter_factories`
/// lookup (`QPDF_Stream.cc:419-435`), before `/DecodeParms` is inspected.
pub(crate) fn validate_filter_factories<'a, I>(names: I) -> Result<()>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    for name in names {
        let normalized = normalize_filter_name(name);
        if stream_filter_for(normalized).is_none() {
            return Err(undecodable_filter_error(normalized));
        }
    }
    Ok(())
}

/// `QPDF_Stream::filterable`'s `warn("stream filter type is not name or
/// array")` (`libqpdf/QPDF_Stream.cc:413`). flpdf raises the same text as an
/// error instead of a warning; see plan decision D3 of `flpdf-25kg.3.4`.
pub(crate) const FILTER_TYPE_ERROR: &str = "stream filter type is not name or array";

/// `QPDF_Stream::filterable`'s `warn("stream /DecodeParms length is
/// inconsistent with filters")` (`libqpdf/QPDF_Stream.cc:459`), raised as an
/// error rather than a warning just as [`FILTER_TYPE_ERROR`] is.
///
/// qpdf validates every filter name against `filter_factories` first and
/// returns on an unknown one (`QPDF_Stream.cc:433-435`), so `:459`'s condition
/// is never evaluated for a stream whose `/Filter` names an unimplemented
/// codec. The handle reader makes the same factory decision before reading
/// `/DecodeParms`, through [`validate_filter_factories`].
pub(crate) const DECODE_PARMS_LENGTH_ERROR: &str =
    "stream /DecodeParms length is inconsistent with filters";

/// Reject a `/Filter` chain longer than `maximum`.
///
/// Unlike qpdf, which caps nothing here, flpdf refuses pathological chains on
/// the decode path; `filters::MAX_FILTER_CHAIN_LEN` documents that divergence.
///
/// The handle reader calls this before it copies an array, so the cap's
/// *body* — the comparison and the message — has exactly one definition.
pub(crate) fn validate_filter_chain_count(count: usize, maximum: Option<usize>) -> Result<()> {
    if let Some(maximum) = maximum.filter(|maximum| count > *maximum) {
        return Err(Error::Unsupported(format!(
            "filter chain length {count} exceeds maximum of {maximum}"
        )));
    }
    Ok(())
}

/// Read `/Filter` and `/DecodeParms` through the resolving `try_*` accessors.
///
/// `QPDF_Stream::filterable` reaches `/Filter` and `/DecodeParms` through
/// `stream_dict.getKey` (`libqpdf/QPDF_Stream.cc:386`, `:441`) and their
/// members through `getArrayItem` (`:400`, `:448`), then inspects each with
/// `isNull`/`isName`/`isArray`/`isInteger` — every one of which dereferences
/// through the owning `QPDF` first. So an indirect child is read as the object
/// it points at, which a `&Object` walk cannot do and which the 2026-08-03
/// live-qpdf probe recorded in plan decision D1 of `flpdf-25kg.3.4`.
///
/// That is unconditional for `/Filter`, each `/Filter` array item, the
/// `/DecodeParms` handle, and each `/DecodeParms` array item — every position
/// `QPDF_Stream::filterable` itself inspects. It is *conditional* one level
/// deeper: a `/DecodeParms` dictionary **value** is reached only by
/// `SF_FlateLzwDecode::setDecodeParms`, so this reader preserves the native
/// handle and lets that setter resolve its values at the qpdf boundary.
///
/// A missing key arrives here as a null handle, exactly as `getKey` hands one
/// back (`libqpdf/QPDFObjectHandle.cc:979-988`), so absent and null share the
/// `isNull` branch just as they do in qpdf.
///
/// This is the canonical production reader for filter metadata. The former
/// Dictionary/value adapters and object-shaped production reader were removed
/// with the legacy object-model route (`d18ce346`). Everything downstream of
/// [`FilterSpec`] — the codec stack, predictor geometry, limits, and warning
/// ordering — stays a single copy.
pub(crate) fn decode_filter_specs_from_handle(
    filter: &ObjectHandle,
    decode_params: &ObjectHandle,
    max_filter_chain: Option<usize>,
) -> Result<Vec<FilterSpec>> {
    let names: Vec<Vec<u8>> = if filter.try_is_null()? {
        return Ok(Vec::new());
    } else if let Some(name) = filter.try_as_name()? {
        vec![name]
    } else if let Some(count) = filter.try_array_len()? {
        // Counted through `try_array_len`, not `try_as_array`, so a chain the
        // cap is about to reject is never copied — qpdf sizes this loop
        // with `getArrayNItems` (`libqpdf/QPDF_Stream.cc:398`), which reads
        // the length off the borrowed array in place. The copy below
        // therefore only happens once the count is known to be acceptable.
        validate_filter_chain_count(count, max_filter_chain)?;
        // `try_array_len` already answered `Some`, so this cannot be `None`;
        // `flatten` states that without a panicking `expect`.
        filter
            .try_as_array()?
            .into_iter()
            .flatten()
            .map(|item| {
                item.try_as_name()?
                    .ok_or_else(|| Error::Unsupported(FILTER_TYPE_ERROR.to_string()))
            })
            .collect::<Result<_>>()?
    } else {
        return Err(Error::Unsupported(FILTER_TYPE_ERROR.to_string()));
    };

    if names.is_empty() {
        return Ok(Vec::new());
    }

    validate_filter_factories(names.iter().map(Vec::as_slice))?;

    let params: Vec<ObjectHandle> = if decode_params.try_is_null()? {
        (0..names.len()).map(|_| ObjectHandle::null()).collect()
    } else if let Some(count) = decode_params.try_array_len()? {
        // Same length-before-copy shape as the `/Filter` arm: qpdf sizes
        // this loop with `getArrayNItems` as well (`libqpdf/QPDF_Stream.cc:443`
        // for the empty-array reduction, `:447` for the per-index walk), and
        // both the empty reduction and the length mismatch are decided from
        // the count alone — so a mismatched array is rejected without being
        // copied.
        if count == 0 {
            (0..names.len()).map(|_| ObjectHandle::null()).collect()
        } else {
            if count != names.len() {
                return Err(Error::Unsupported(DECODE_PARMS_LENGTH_ERROR.to_string()));
            }
            decode_params
                .try_as_array()?
                .into_iter()
                .flatten()
                .collect()
        }
    } else {
        // One handle replicated across the chain, exactly as qpdf pushes the
        // same `QPDFObjectHandle` per filter (`QPDF_Stream.cc:450-454`).
        (0..names.len()).map(|_| decode_params.clone()).collect()
    };

    validate_filter_chain_count(names.len(), max_filter_chain)?;

    Ok(names
        .into_iter()
        .zip(params)
        .map(|(name, decode_params)| FilterSpec {
            name,
            decode_params,
        })
        .collect())
}

struct OutputBuffer {
    data: Vec<u8>,
    max_output: Option<usize>,
    cleanup_data_start: Option<usize>,
    finish_phase: Rc<Cell<bool>>,
    output_position: Rc<Cell<usize>>,
}

impl OutputBuffer {
    fn new(max_output: Option<usize>) -> Self {
        Self {
            data: Vec::new(),
            max_output,
            cleanup_data_start: None,
            finish_phase: Rc::new(Cell::new(false)),
            output_position: Rc::new(Cell::new(0)),
        }
    }

    fn finish_phase(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.finish_phase)
    }

    fn output_position(&self) -> Rc<Cell<usize>> {
        Rc::clone(&self.output_position)
    }

    fn cleanup_data_start(&self) -> usize {
        self.cleanup_data_start.unwrap_or(self.data.len())
    }
}

impl Pipeline for OutputBuffer {
    fn identifier(&self) -> &str {
        "stream data buffer"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.finish_phase.get() && self.cleanup_data_start.is_none() {
            self.cleanup_data_start = Some(self.data.len());
        }
        if let Some(limit) = self.max_output {
            let remaining = limit.saturating_sub(self.data.len());
            if data.len() > remaining {
                self.data.extend_from_slice(&data[..remaining]);
                self.output_position.set(self.data.len());
                return Err(PipelineError::runtime(format!(
                    "{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"
                )));
            }
        }
        self.data.extend_from_slice(data);
        self.output_position.set(self.data.len());
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn map_pipeline_error(error: PipelineError) -> Error {
    Error::Unsupported(error.into_string_lossy())
}

pub(crate) struct FilterDecodeOutcome {
    pub(crate) data: Vec<u8>,
    pub(crate) cleanup_data_start: usize,
    pub(crate) error: Option<FilterDecodeError>,
}

pub(crate) struct FilterDecodeError {
    pub(crate) error: Error,
    pub(crate) during_write: bool,
    pub(crate) output_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilterDecodePhase {
    Write,
    Finish,
}

/// Pipe one complete encoded buffer through a stage with qpdf's error cleanup.
///
/// `QPDF::pipeStreamData` calls `finish` after a failed `write`, ignores that
/// secondary result, and keeps the original exception. The stage's sink remains
/// owned by its caller, so bytes forwarded before the failure stay accessible.
struct StagePipelineError {
    error: PipelineError,
    during_write: bool,
    output_offset: usize,
}

fn write_and_finish(
    stage: &mut dyn Pipeline,
    data: &[u8],
    finish_phase: Option<&Cell<bool>>,
    output_position: &Cell<usize>,
) -> Option<StagePipelineError> {
    if let Some(finish_phase) = finish_phase {
        finish_phase.set(false);
    }
    match stage.write(data) {
        Ok(()) => {
            if let Some(finish_phase) = finish_phase {
                finish_phase.set(true);
            }
            stage.finish().err().map(|error| StagePipelineError {
                error,
                during_write: false,
                output_offset: output_position.get(),
            })
        }
        Err(error) => {
            let output_offset = output_position.get();
            if let Some(finish_phase) = finish_phase {
                finish_phase.set(true);
            }
            let _ = stage.finish();
            Some(StagePipelineError {
                error,
                during_write: true,
                output_offset,
            })
        }
    }
}

fn map_stage_error(error: StagePipelineError) -> FilterDecodeError {
    FilterDecodeError {
        error: map_pipeline_error(error.error),
        during_write: error.during_write,
        output_offset: error.output_offset,
    }
}

/// Rust equivalent of qpdf's `QPDFStreamFilter` extension boundary.
///
/// `pipe_decode_recovering` owns construction and completion of the filter's
/// decode pipeline. A whole-buffer result keeps the legacy decode helpers
/// stable while the individual codecs use incremental `Pipeline` stages.
pub(crate) trait StreamFilter {
    /// Port of `QPDFStreamFilter::setDecodeParms`
    /// (`libqpdf/QPDFStreamFilter.cc:3-7`), whose whole body is
    /// `return decode_parms.isNull();` — documented at
    /// `include/qpdf/QPDFStreamFilter.hh:41-42` as "The default implementation
    /// accepts a null object and rejects everything else". A missing
    /// `/DecodeParms` key is represented by the same null `ObjectHandle`.
    fn set_decode_params(&mut self, decode_params: &ObjectHandle) -> Result<bool> {
        decode_params.try_is_null()
    }

    /// Build the filter's decode pipeline without decoding anything.
    ///
    /// `QPDF_Stream::pipeStreamData` constructs every filter's decode pipeline
    /// before it writes the first byte, so a stage whose parameters cannot form
    /// a pipeline is rejected even when an earlier stage would have failed on
    /// the data itself.
    fn preflight_decode_pipeline(&self) -> Result<()> {
        Ok(())
    }

    /// Install the optional qpdf-head TIFF row-memory budget before preflight
    /// and execution. Other filters ignore this setting.
    fn set_tiff_memory_limit(&mut self, _limit: Option<usize>) {}

    /// Construct the same stage with a downstream pipeline that may already
    /// own inner stages. This is the Rust ownership seam used by
    /// `QPDF_Stream::pipeStreamData`'s reverse chain construction.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>>;

    /// Install the qpdf `QPDF_Stream::pipeStreamData` warning callback on a
    /// filter that constructs a Flate stage. Other filters ignore it.
    fn set_warning_callback(&mut self, _callback: FilterWarningCallback) {}

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome>;

    /// Whether this filter is a specialized compression codec for qpdf's
    /// stream capability classification.
    fn is_specialized_compression(&self) -> bool {
        false
    }

    /// Whether this filter is a lossy compression codec for qpdf's stream
    /// capability classification.
    fn is_lossy_compression(&self) -> bool {
        false
    }
}

/// Result of constructing a stage around a downstream pipeline that may
/// already own inner stages. `NoStage` returns the downstream slot so the
/// caller can keep threading it through a filter such as qpdf's `Crypt`.
pub(crate) enum OwnedDecodePipeline<'a> {
    Stage(Box<dyn Pipeline + 'a>),
    NoStage(PipelineRef<'a>),
}

/// Rust equivalent of qpdf's `SF_FlateLzwDecode`.
///
/// One filter serves `FlateDecode` and `LZWDecode`, owns the shared predictor
/// parameters, and builds the decode chain codec-then-predictor.
struct FlateLzwStreamFilter {
    lzw: bool,
    predictor: i32,
    columns: i32,
    colors: i32,
    bits_per_component: i32,
    early_code_change: bool,
    warning_callback: Option<FilterWarningCallback>,
    tiff_max_memory: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PredictorKind {
    Png,
    Tiff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PredictorGeometry {
    kind: PredictorKind,
    columns: u32,
    colors: u32,
    bits_per_component: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PredictorAction {
    Encode,
    Decode,
}

impl FlateLzwStreamFilter {
    /// Construct with the PDF specification defaults qpdf uses.
    fn new(lzw: bool) -> Self {
        Self {
            lzw,
            predictor: 1,
            columns: 1,
            colors: 1,
            bits_per_component: 8,
            early_code_change: true,
            warning_callback: None,
            tiff_max_memory: None,
        }
    }
}

/// Mirror `QIntC::to_uint`, whose range failure is a `std::runtime_error`.
fn to_uint(value: i32) -> Result<u32> {
    u32::try_from(value).map_err(|_| {
        Error::Unsupported(format!(
            "integer out of range converting {value} from a 4-byte signed type to a 4-byte unsigned type"
        ))
    })
}

impl StreamFilter for FlateLzwStreamFilter {
    fn set_decode_params(&mut self, decode_params: &ObjectHandle) -> Result<bool> {
        // The one early return SF_FlateLzwDecode::setDecodeParms has
        // (SF_FlateLzwDecode.cc:24-26), for a null /DecodeParms. Every other
        // shape walks the keys and then falls through to the trailing check at
        // :68-70, which a present-but-empty parameter set reaches.
        if decode_params.try_is_null()? {
            return Ok(true);
        }

        let mut filterable = true;
        for key in decode_params.try_get_keys()? {
            let value = decode_params.try_get_key(&key)?;
            let key = key.as_slice();
            match key {
                b"/Predictor" => {
                    let Some(_) = value.try_as_integer()? else {
                        filterable = false;
                        continue;
                    };
                    let predictor = value.try_get_int_value_as_int()?;
                    self.predictor = predictor;
                    if !((predictor == 1) || (predictor == 2) || (10..=15).contains(&predictor)) {
                        filterable = false;
                    }
                }
                b"/Columns" | b"/Colors" | b"/BitsPerComponent" => {
                    let Some(_) = value.try_as_integer()? else {
                        filterable = false;
                        continue;
                    };
                    // qpdf stores these without range validation and defers
                    // rejection to pipeline construction.
                    let parameter = value.try_get_int_value_as_int()?;
                    match key {
                        b"/Columns" => self.columns = parameter,
                        b"/Colors" => self.colors = parameter,
                        _ => self.bits_per_component = parameter,
                    }
                }
                // qpdf consults /EarlyChange only for LZW streams.
                b"/EarlyChange" if self.lzw => {
                    let Some(_) = value.try_as_integer()? else {
                        filterable = false;
                        continue;
                    };
                    let early_change = value.try_get_int_value_as_int()?;
                    self.early_code_change = early_change == 1;
                    if !((early_change == 0) || (early_change == 1)) {
                        filterable = false;
                    }
                }
                _ => {}
            }
        }

        if (self.predictor > 1) && (self.columns == 0) {
            filterable = false;
        }

        Ok(filterable)
    }

    fn set_warning_callback(&mut self, callback: FilterWarningCallback) {
        self.warning_callback = Some(callback);
    }

    fn preflight_decode_pipeline(&self) -> Result<()> {
        if let Some(geometry) = self.decode_predictor_geometry()? {
            let mut sink = OutputBuffer::new(None);
            let _predictor = make_predictor_pipeline(
                geometry,
                &mut sink,
                PredictorAction::Decode,
                self.tiff_max_memory,
            )?;
        }
        Ok(())
    }

    fn set_tiff_memory_limit(&mut self, limit: Option<usize>) {
        self.tiff_max_memory = limit;
    }

    /// Mirrors `SF_FlateLzwDecode::getDecodePipeline`
    /// (`libqpdf/SF_FlateLzwDecode.cc:75-110`): a predictor stage first when
    /// the parameters call for one, with `next` reassigned to it, then the
    /// codec wrapping whichever `next` resulted. The codec is what the caller
    /// receives.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        let next: PipelineRef<'a> = match self.decode_predictor_geometry()? {
            Some(geometry) => make_predictor_pipeline(
                geometry,
                next,
                PredictorAction::Decode,
                self.tiff_max_memory,
            )?,
            None => next,
        };
        let stage: Box<dyn Pipeline + 'a> = if self.lzw {
            Box::new(LzwDecoder::new("lzw decode", next, self.early_code_change))
        } else {
            let mut flate = Flate::new(
                "stream inflate",
                next,
                FlateAction::Inflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )
            .map_err(map_pipeline_error)?;
            if let Some(callback) = self.warning_callback.take() {
                flate.set_warn_callback(callback);
            }
            Box::new(flate)
        };
        Ok(OwnedDecodePipeline::Stage(stage))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        let geometry = self.decode_predictor_geometry()?;
        let mut sink = OutputBuffer::new(max_output);
        let finish_phase = sink.finish_phase();
        let output_position = sink.output_position();
        // SF_FlateLzwDecode::getDecodePipeline builds the chain from the sink
        // outward, so the predictor stage is constructed before the codec and
        // any construction failure precedes every codec write.
        let error = match geometry {
            Some(geometry) => {
                let mut predictor = make_predictor_pipeline(
                    geometry,
                    &mut sink,
                    PredictorAction::Decode,
                    self.tiff_max_memory,
                )?;
                let phase = Some(finish_phase.as_ref());
                self.pipe_codec(&mut predictor, data, warn, phase, &output_position)?
            }
            None => {
                self.pipe_codec(&mut sink, data, warn, Some(&finish_phase), &output_position)?
            }
        };
        Ok(FilterDecodeOutcome {
            cleanup_data_start: sink.cleanup_data_start(),
            data: sink.data,
            error,
        })
    }
}

impl FlateLzwStreamFilter {
    /// Resolve the predictor geometry the decode chain needs, if any.
    ///
    /// This reproduces the failures `SF_FlateLzwDecode::getDecodePipeline`
    /// raises while constructing the chain, so both the preflight and the
    /// decode itself reject exactly the same parameters.
    fn decode_predictor_geometry(&self) -> Result<Option<PredictorGeometry>> {
        let kind = if (10..=15).contains(&self.predictor) {
            Some(PredictorKind::Png)
        } else if self.predictor == 2 {
            Some(PredictorKind::Tiff)
        } else {
            None
        };
        kind.map(|kind| {
            Ok(PredictorGeometry {
                kind,
                columns: to_uint(self.columns)?,
                colors: to_uint(self.colors)?,
                bits_per_component: to_uint(self.bits_per_component)?,
            })
        })
        .transpose()
    }

    /// Run the codec stage of the whole-buffer route over `data`.
    ///
    /// **Recorded deviation:** the `Pl_Flate` warn callback is installed here,
    /// on the stage this function constructs, where qpdf installs it at the
    /// `getDecodePipeline` caller (`QPDF_Stream.cc:564-567`). Every `Pl_Flate`
    /// this route builds still receives the callback qpdf would install at
    /// that filter's own iteration, so warning text and order are unchanged.
    /// What installing it here cannot reproduce is qpdf's other case: the cast
    /// runs once per filter rather than once per constructed stage, so an
    /// iteration whose filter builds nothing lands it on a stage constructed
    /// elsewhere — see qpdf's `getDecodePipeline` boundary. Both that case and
    /// the placement belong with the port of `QPDF_Stream::pipeStreamData`.
    ///
    /// Nothing today can observe the difference: this route decodes each
    /// filter's whole buffer in one call, constructing and finishing that
    /// filter's stages within it, so no chain head survives into the next
    /// filter — and `Crypt`, the only filter whose `decode_pipeline` builds
    /// nothing, never reaches this function at all, because
    /// `filters::prepare_decode_filters` routes its spec to
    /// `PreparedStage::Crypt` instead of to a codec adapter.
    fn pipe_codec(
        &self,
        next: &mut dyn Pipeline,
        data: &[u8],
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
        finish_phase: Option<&Cell<bool>>,
        output_position: &Cell<usize>,
    ) -> Result<Option<FilterDecodeError>> {
        let error = if self.lzw {
            let mut stage = LzwDecoder::new("lzw decode", next, self.early_code_change);
            write_and_finish(&mut stage, data, finish_phase, output_position)
        } else {
            let mut stage = Flate::new(
                "stream inflate",
                next,
                FlateAction::Inflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )
            .map_err(map_pipeline_error)?;
            stage.set_warn_callback(|message, code| {
                let phase = filter_decode_phase(finish_phase);
                warn(message, code, output_position.get(), phase)
            });
            write_and_finish(&mut stage, data, finish_phase, output_position)
        };
        Ok(error.map(map_stage_error))
    }
}

fn make_predictor_pipeline<'a>(
    geometry: PredictorGeometry,
    next: impl Into<PipelineRef<'a>>,
    action: PredictorAction,
    tiff_max_memory: Option<usize>,
) -> Result<PipelineRef<'a>> {
    let next = next.into();
    let pipeline = match (geometry.kind, action) {
        (PredictorKind::Png, PredictorAction::Encode) => Box::new(
            PngFilter::new(
                "png encode",
                next,
                PngFilterAction::Encode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Png, PredictorAction::Decode) => Box::new(
            PngFilter::new(
                "png decode",
                next,
                PngFilterAction::Decode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Tiff, PredictorAction::Encode) => Box::new(
            TiffPredictor::new_with_memory_limit(
                "tiff encode",
                next,
                TiffPredictorAction::Encode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
                tiff_max_memory,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Tiff, PredictorAction::Decode) => Box::new(
            TiffPredictor::new_with_memory_limit(
                "tiff decode",
                next,
                TiffPredictorAction::Decode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
                tiff_max_memory,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
    };
    Ok(PipelineRef::Owned(pipeline))
}

fn filter_decode_phase(finish_phase: Option<&Cell<bool>>) -> FilterDecodePhase {
    if finish_phase.is_some_and(Cell::get) {
        FilterDecodePhase::Finish
    } else {
        FilterDecodePhase::Write
    }
}

struct Ascii85StreamFilter;

impl StreamFilter for Ascii85StreamFilter {
    /// Mirrors `SF_ASCII85Decode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_ASCII85Decode.hh:14-19`), a single `Pl_ASCII85Decoder`.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(Ascii85Decoder::new(
            "ascii85 decode",
            next,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        decode_ascii85(data, max_output)
    }
}

struct AsciiHexStreamFilter;

impl StreamFilter for AsciiHexStreamFilter {
    /// Mirrors `SF_ASCIIHexDecode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_ASCIIHexDecode.hh:14-19`), a single
    /// `Pl_ASCIIHexDecoder`.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(AsciiHexDecoder::new(
            "asciiHex decode",
            next,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        decode_ascii_hex(data, max_output)
    }
}

struct RunLengthStreamFilter;

impl StreamFilter for RunLengthStreamFilter {
    /// Mirrors `SF_RunLengthDecode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_RunLengthDecode.hh:14-20`), a single `Pl_RunLength`
    /// in its decode action.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(RunLength::new(
            "runlength decode",
            next,
            RunLengthAction::Decode,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        decode_run_length(data, max_output)
    }

    fn is_specialized_compression(&self) -> bool {
        true
    }
}

struct DctStreamFilter;

impl StreamFilter for DctStreamFilter {
    /// Mirrors `SF_DCTDecode::getDecodePipeline`
    /// (`libqpdf/qpdf/SF_DCTDecode.hh:14-19`), a single `Pl_DCT` decode stage.
    fn decode_pipeline_owned<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(PlDct::new(
            "DCT decode",
            next,
        ))))
    }

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        let mut sink = OutputBuffer::new(max_output);
        let finish_phase = sink.finish_phase();
        let output_position = sink.output_position();
        let error = {
            let mut stage = PlDct::new("DCT decode", &mut sink).with_max_output(max_output);
            write_and_finish(
                &mut stage,
                data,
                Some(finish_phase.as_ref()),
                &output_position,
            )
            .map(map_stage_error)
        };
        let cleanup_data_start = sink.cleanup_data_start();
        Ok(FilterDecodeOutcome {
            data: sink.data,
            cleanup_data_start,
            error,
        })
    }

    fn is_specialized_compression(&self) -> bool {
        true
    }

    fn is_lossy_compression(&self) -> bool {
        true
    }
}

/// Port of the anonymous-namespace `SF_Crypt` in `libqpdf/QPDF_Stream.cc:27-58`.
///
/// It decodes nothing. Its whole contribution is deciding filterability from
/// `/DecodeParms`, which is why qpdf lists it in `filter_factories`
/// (`QPDF_Stream.cc:85-94`) beside that table's six codec filters even though
/// its `getDecodePipeline` returns `nullptr`.
struct CryptStreamFilter;

impl StreamFilter for CryptStreamFilter {
    /// Port of `SF_Crypt::setDecodeParms` (`QPDF_Stream.cc:33-50`).
    ///
    /// Every key must be `Type` or `Name`, and a present `Type` must name
    /// `/CryptFilterDecodeParms`. Observed against qpdf 11.9.0 on 2026-08-08
    /// through `qpdf --show-object=4 --filtered-stream-data` over a stream
    /// whose `/Filter` is `/Crypt`: `<< /Name /Identity >>`,
    /// `<< /Type /CryptFilterDecodeParms >>` and `<< >>` exit 0, while
    /// `<< /Type /Foo >>`, `<< /Foo 1 >>` and
    /// `<< /Type /CryptFilterDecodeParms /Foo 1 >>` exit 2 with "unable to
    /// filter stream data".
    ///
    /// The `Type` validity test is evaluated *inside* the loop, as qpdf
    /// evaluates it, even though hoisting the predicate would answer
    /// identically on every shape — qpdf's loop shape is kept deliberately,
    /// not because behaviour depends on it.
    fn set_decode_params(&mut self, decode_params: &ObjectHandle) -> Result<bool> {
        if decode_params.try_is_null()? {
            return Ok(true);
        }
        let mut filterable = true;
        for key in decode_params.try_get_keys()? {
            let is_allowed_key = (key.as_slice() == b"/Type") || (key.as_slice() == b"/Name");
            if is_allowed_key
                && (!decode_params.try_has_key(b"/Type")?
                    || decode_params.try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")?)
            {
                // qpdf handles these two in decryptStream.
            } else {
                filterable = false;
            }
        }
        Ok(filterable)
    }

    /// Port of `SF_Crypt::getDecodePipeline` (`QPDF_Stream.cc:52-56`), whose
    /// whole body returns `nullptr`: a `Crypt` stage contributes no decode
    /// stage, because decryption happens in `decryptStream` instead.
    ///
    /// A caller that installs this `None` must therefore already be reading
    /// through a decrypting source; qpdf's filter loop
    /// (`QPDF_Stream.cc:559-568`) runs after `decryptStream` has been applied
    /// to the source bytes. Without such a source the stage is not merely
    /// absent but wrong, and silently so — ciphertext would pass through as
    /// plaintext with neither an error nor a warning, which is why the
    /// decode route below refuses instead of returning the bytes.
    fn decode_pipeline_owned<'a>(
        &mut self,
        _next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::NoStage(_next))
    }

    /// Refuse to decode, reporting [`CRYPT_STAGE_UNSUPPORTED`].
    ///
    /// qpdf has no counterpart to mirror: `SF_Crypt` contributes no pipeline
    /// and decryption is `decryptStream`'s job, so this route is flpdf's
    /// alone. Nothing reaches it today —
    /// `filters::prepare_decode_filters` routes a `Crypt` spec to
    /// `PreparedStage::Crypt` before the registry is consulted, and the crypt
    /// provider every non-decrypting entry point installs is
    /// `filters::reject_crypt_stage`. Sharing that provider's message — the
    /// same constant, not a second copy of it — is what keeps the public error
    /// unchanged if decoding is ever routed here instead.
    // qpdf-deviation: no qpdf counterpart -- QPDFStreamFilter has no
    // execute-time decode call (only setDecodeParms/getDecodePipeline), so
    // this call shape (invoking a Crypt stage's decode step directly) is one
    // qpdf can never produce; this refusal guards flpdf's own registry route
    // rather than reproducing any qpdf behavior.
    fn pipe_decode_recovering(
        &mut self,
        _data: &[u8],
        _max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Err(Error::Unsupported(CRYPT_STAGE_UNSUPPORTED.to_string()))
    }
}

/// Construct the filter registered under `filter_name`, if any.
///
/// **Recorded deviation (CLAUDE.md class (B)):** qpdf holds the same registry
/// in a `std::map`, `QPDF_Stream::filter_factories` (`QPDF_Stream.cc:85-94`).
/// Nothing iterates that map — the only read is a lookup by name
/// (`QPDF_Stream.cc:425-426`) — so a `match` carries a name-to-factory
/// mapping just as faithfully. What a `match` cannot carry is
/// `QPDF_Stream::registerStreamFilter` (`QPDF_Stream.cc:148-151`), which lets
/// a library user add a factory at run time; flpdf exposes no counterpart, and
/// adding one would mean replacing this `match`.
///
/// The container and qpdf's registered production codecs are represented here;
/// the DCT stage itself is the qpdf-shaped streaming primitive, and the
/// whole-buffer adapter below drives that same stage for legacy callers.
pub(crate) fn stream_filter_for(filter_name: &[u8]) -> Option<Box<dyn StreamFilter>> {
    match filter_name {
        b"Crypt" => Some(Box::new(CryptStreamFilter)),
        b"FlateDecode" => Some(Box::new(FlateLzwStreamFilter::new(false))),
        b"LZWDecode" => Some(Box::new(FlateLzwStreamFilter::new(true))),
        b"ASCII85Decode" => Some(Box::new(Ascii85StreamFilter)),
        b"ASCIIHexDecode" => Some(Box::new(AsciiHexStreamFilter)),
        b"RunLengthDecode" => Some(Box::new(RunLengthStreamFilter)),
        b"DCTDecode" => Some(Box::new(DctStreamFilter)),
        _ => None,
    }
}

fn decode_ascii85(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let output_position = sink.output_position();
    let error = {
        let mut stage = Ascii85Decoder::new("ascii85 decode", &mut sink);
        write_and_finish(&mut stage, data, Some(&finish_phase), &output_position)
            .map(map_stage_error)
    };
    Ok(FilterDecodeOutcome {
        cleanup_data_start: sink.cleanup_data_start(),
        data: sink.data,
        error,
    })
}

fn decode_ascii_hex(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let output_position = sink.output_position();
    let error = {
        let mut stage = AsciiHexDecoder::new("asciiHex decode", &mut sink);
        write_and_finish(&mut stage, data, Some(&finish_phase), &output_position)
            .map(map_stage_error)
    };
    Ok(FilterDecodeOutcome {
        cleanup_data_start: sink.cleanup_data_start(),
        data: sink.data,
        error,
    })
}

fn decode_run_length(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let output_position = sink.output_position();
    let error = {
        let mut stage = RunLength::new("runlength decode", &mut sink, RunLengthAction::Decode);
        write_and_finish(&mut stage, data, Some(&finish_phase), &output_position)
            .map(map_stage_error)
    };
    Ok(FilterDecodeOutcome {
        cleanup_data_start: sink.cleanup_data_start(),
        data: sink.data,
        error,
    })
}

pub(crate) fn encode_flate(data: &[u8]) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut flate = Flate::new(
            "compress stream",
            &mut sink,
            FlateAction::Deflate,
            DEFAULT_OUT_BUFFER_SIZE,
        )
        .map_err(map_pipeline_error)?;
        flate.write(data).map_err(map_pipeline_error)?;
        flate.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

/// Resolve the predictor geometry a writer must apply for `/DecodeParms`.
///
/// Returns `Ok(None)` when the parameters select no predictor. The
/// parameters are validated through the same `SF_FlateLzwDecode` state the
/// decode path uses, so both directions accept exactly the same dictionaries.
fn predictor_encode_geometry(
    filter_name: &[u8],
    decode_params: &ObjectHandle,
) -> Result<Option<PredictorGeometry>> {
    // qpdf's default QPDFStreamFilter::setDecodeParms accepts only a null
    // object. ASCII85, ASCIIHex, and RunLength inherit that contract; only
    // SF_FlateLzwDecode consumes predictor parameters. Validate the registered
    // non-Flate filter here so encoding cannot produce bytes for a stream that
    // the inverse decode path rejects.
    if !matches!(filter_name, b"FlateDecode" | b"LZWDecode") {
        let Some(mut filter) = stream_filter_for(filter_name) else {
            // Let the codec encoder report an unknown or passthrough filter.
            return Ok(None);
        };
        if !filter.set_decode_params(decode_params)? {
            return Err(Error::Unsupported(format!(
                "stream filter {} does not support supplied /DecodeParms",
                String::from_utf8_lossy(filter_name)
            )));
        }
        return Ok(None);
    }

    let mut filter = FlateLzwStreamFilter::new(filter_name == b"LZWDecode");
    if !filter.set_decode_params(decode_params)? {
        return Err(Error::Unsupported(format!(
            "stream filter {} does not support supplied /DecodeParms",
            String::from_utf8_lossy(filter_name)
        )));
    }
    filter.decode_predictor_geometry()
}

/// Apply the predictor selected by `/DecodeParms` before a codec's encode step.
pub(crate) fn encode_predictor(
    data: &[u8],
    filter_name: &[u8],
    decode_params: &ObjectHandle,
) -> Result<Vec<u8>> {
    let Some(geometry) = predictor_encode_geometry(filter_name, decode_params)? else {
        return Ok(data.to_vec());
    };
    encode_predictor_stage(data, geometry)
}

fn encode_predictor_stage(data: &[u8], geometry: PredictorGeometry) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut predictor =
            make_predictor_pipeline(geometry, &mut sink, PredictorAction::Encode, None)?;
        predictor.write(data).map_err(map_pipeline_error)?;
        predictor.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

pub(crate) fn encode_run_length(data: &[u8]) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut stage = RunLength::new("compress stream", &mut sink, RunLengthAction::Encode);
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

#[cfg(test)]
mod tests {
    use super::{FlateLzwStreamFilter, StreamFilter};
    use crate::pipeline::test_support::RecordingSink;
    use crate::pipeline::PipelineRef;
    use crate::ObjectHandle;

    fn wide_tiff_decode_params() -> ObjectHandle {
        ObjectHandle::dictionary(vec![
            (b"/Predictor".to_vec(), ObjectHandle::integer(2)),
            (b"/Columns".to_vec(), ObjectHandle::integer(536_870_911)),
            (b"/Colors".to_vec(), ObjectHandle::integer(1)),
            (b"/BitsPerComponent".to_vec(), ObjectHandle::integer(8)),
        ])
    }

    #[test]
    fn flate_filter_consumes_qpdf_canonical_slash_prefixed_keys() {
        let mut filter = FlateLzwStreamFilter::new(false);
        let params =
            ObjectHandle::dictionary(vec![(b"/Predictor".to_vec(), ObjectHandle::integer(2))]);

        assert!(filter.set_decode_params(&params).unwrap());
        assert_eq!(filter.predictor, 2);
    }

    #[test]
    fn flate_reader_preserves_canonical_key_matching_and_ignores_raw_unknown_keys() {
        fn decode_params(key: &[u8]) -> ObjectHandle {
            let params = ObjectHandle::dictionary(vec![]);
            params
                .replace_key(key, ObjectHandle::integer(2))
                .expect("decode parameter dictionary is mutable");
            params
        }

        let canonical = decode_params(b"/Predictor");
        let raw = decode_params(b"Predictor");
        let special_unknown = decode_params(b"/Predictor B");

        let mut canonical_filter = FlateLzwStreamFilter::new(false);
        assert!(canonical_filter.set_decode_params(&canonical).unwrap());
        assert_eq!(canonical_filter.predictor, 2);

        let mut raw_filter = FlateLzwStreamFilter::new(false);
        assert!(raw_filter.set_decode_params(&raw).unwrap());
        assert_eq!(raw_filter.predictor, 1);

        let mut special_filter = FlateLzwStreamFilter::new(false);
        assert!(special_filter.set_decode_params(&special_unknown).unwrap());
        assert_eq!(special_filter.predictor, 1);
    }

    #[test]
    fn flate_setter_warns_only_when_qpdf_reads_an_integer_parameter() {
        let (huge, recorder) = crate::object_handle::warning_emission_tests::handle_resolving(
            crate::object_handle::ObjectValue::Integer(i64::MAX),
        );
        let params = ObjectHandle::dictionary(vec![
            (b"Columns".to_vec(), huge.clone()),
            (b"Foo".to_vec(), huge.clone()),
            (b"EarlyChange".to_vec(), huge),
        ]);
        let mut filter = FlateLzwStreamFilter::new(false);

        assert!(filter.set_decode_params(&params).unwrap());
        assert_eq!(
            crate::object_handle::warning_emission_tests::warnings(&recorder),
            vec!["object 3 0: requested value of integer is too big; returning INT_MAX"]
        );
    }

    #[test]
    fn filter_specs_keep_scalar_and_array_parameter_handle_mutations() {
        let filter = ObjectHandle::name(b"FlateDecode".to_vec());
        let scalar_params =
            ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), ObjectHandle::integer(1))]);
        let scalar_specs =
            super::decode_filter_specs_from_handle(&filter, &scalar_params, None).unwrap();
        scalar_params
            .replace_key(b"/Predictor", ObjectHandle::integer(2))
            .unwrap();
        let mut scalar_filter = FlateLzwStreamFilter::new(false);
        assert!(scalar_filter
            .set_decode_params(&scalar_specs[0].decode_params)
            .unwrap());
        assert_eq!(scalar_filter.predictor, 2);

        let array_item =
            ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), ObjectHandle::integer(1))]);
        let filter_array = ObjectHandle::array(vec![filter]);
        let params_array = ObjectHandle::array(vec![array_item.clone()]);
        let array_specs =
            super::decode_filter_specs_from_handle(&filter_array, &params_array, None).unwrap();
        array_item
            .replace_key(b"/Predictor", ObjectHandle::integer(2))
            .unwrap();
        let mut array_filter = FlateLzwStreamFilter::new(false);
        assert!(array_filter
            .set_decode_params(&array_specs[0].decode_params)
            .unwrap());
        assert_eq!(array_filter.predictor, 2);
    }

    #[test]
    fn base_stream_filter_accepts_only_null_decode_parameters() {
        let mut filter = super::AsciiHexStreamFilter;
        assert!(filter.set_decode_params(&ObjectHandle::null()).unwrap());
        assert!(!filter
            .set_decode_params(&ObjectHandle::dictionary(vec![]))
            .unwrap());
    }

    #[test]
    fn crypt_reader_preserves_qpdf_name_and_type_validation() {
        fn decode_params(entries: Vec<(Vec<u8>, ObjectHandle)>) -> ObjectHandle {
            ObjectHandle::dictionary(entries)
        }

        let mut identity_filter = super::CryptStreamFilter;
        let identity = decode_params(vec![(
            b"/Name".to_vec(),
            ObjectHandle::name(b"Identity".to_vec()),
        )]);
        assert!(identity_filter.set_decode_params(&identity).unwrap());

        let mut valid_type_filter = super::CryptStreamFilter;
        let valid_type = decode_params(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"CryptFilterDecodeParms".to_vec()),
        )]);
        assert!(valid_type_filter.set_decode_params(&valid_type).unwrap());

        let mut unknown_key_filter = super::CryptStreamFilter;
        let unknown_key = decode_params(vec![(b"/Foo".to_vec(), ObjectHandle::integer(1))]);
        assert!(!unknown_key_filter.set_decode_params(&unknown_key).unwrap());

        let mut invalid_type_filter = super::CryptStreamFilter;
        let invalid_type = decode_params(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Foo".to_vec()),
        )]);
        assert!(!invalid_type_filter
            .set_decode_params(&invalid_type)
            .unwrap());
    }

    #[test]
    fn crypt_setter_reads_a_non_dictionary_once_before_accepting_empty_keys() {
        let (params, recorder) = crate::object_handle::warning_emission_tests::handle_resolving(
            crate::object_handle::ObjectValue::Integer(1),
        );
        let mut filter = super::CryptStreamFilter;

        assert!(filter.set_decode_params(&params).unwrap());
        assert_eq!(
            crate::object_handle::warning_emission_tests::warnings(&recorder),
            vec![
                "object 3 0: operation for dictionary attempted on object of type integer: treating as empty"
            ]
        );
    }

    #[test]
    fn lzw_reader_consumes_only_canonical_early_change_keys() {
        fn decode_params(_filter_name: &[u8], key: &[u8], value: i64) -> ObjectHandle {
            let params = ObjectHandle::dictionary(vec![]);
            params
                .replace_key(key, ObjectHandle::integer(value))
                .expect("decode parameter dictionary is mutable");
            params
        }

        for (value, expected_filterable, expected_early_change) in
            [(0, true, false), (1, true, true), (2, false, false)]
        {
            let params = decode_params(b"LZWDecode", b"/EarlyChange", value);
            let mut filter = FlateLzwStreamFilter::new(true);
            assert_eq!(
                filter.set_decode_params(&params).unwrap(),
                expected_filterable
            );
            assert_eq!(filter.early_code_change, expected_early_change);
        }

        let raw = decode_params(b"LZWDecode", b"EarlyChange", 0);
        let mut raw_filter = FlateLzwStreamFilter::new(true);
        assert!(raw_filter.set_decode_params(&raw).unwrap());
        assert!(raw_filter.early_code_change);

        let flate = decode_params(b"FlateDecode", b"/EarlyChange", 0);
        let mut flate_filter = FlateLzwStreamFilter::new(false);
        assert!(flate_filter.set_decode_params(&flate).unwrap());
        assert!(flate_filter.early_code_change);
    }

    fn wide_tiff_filter() -> FlateLzwStreamFilter {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter
            .set_decode_params(&wide_tiff_decode_params())
            .unwrap());
        filter.set_tiff_memory_limit(Some(1 << 20));
        filter
    }

    #[test]
    fn owned_decode_pipeline_applies_tiff_memory_limit_before_codec_construction() {
        let mut filter = wide_tiff_filter();
        let mut sink = RecordingSink::new(&[], &[]);
        let result = filter.decode_pipeline_owned(PipelineRef::from(&mut sink));
        assert!(result.is_err());
        let error = result.err().unwrap();

        assert!(error
            .to_string()
            .contains("TIFFPredictor memory limit exceeded"));
    }

    #[test]
    fn recovering_decode_pipeline_applies_tiff_memory_limit_before_codec_writes() {
        let mut filter = wide_tiff_filter();
        let result = filter.pipe_decode_recovering(&[], None, &mut |_, _, _, _| Ok(()));
        assert!(result.is_err());
        let error = result.err().unwrap();

        assert!(error
            .to_string()
            .contains("TIFFPredictor memory limit exceeded"));
    }

    #[test]
    fn encode_predictor_uses_the_tiff_stream_filter_pipeline() {
        let params = ObjectHandle::dictionary(vec![
            (b"/Predictor".to_vec(), ObjectHandle::integer(2)),
            (b"/Columns".to_vec(), ObjectHandle::integer(2)),
            (b"/Colors".to_vec(), ObjectHandle::integer(1)),
            (b"/BitsPerComponent".to_vec(), ObjectHandle::integer(8)),
        ]);

        assert_eq!(
            super::encode_predictor(&[10, 20], b"FlateDecode", &params).unwrap(),
            [10, 10]
        );
    }
}
