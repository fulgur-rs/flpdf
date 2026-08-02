//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name, DecodeParms-alignment, and decode-pipeline construction responsibilities.

use crate::pipeline::ascii85::Ascii85Decoder;
use crate::pipeline::ascii_hex::AsciiHexDecoder;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
use crate::pipeline::lzw::LzwDecoder;
use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
use crate::pipeline::run_length::{RunLength, RunLengthAction};
use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
use crate::{Dictionary, Error, Object, Result};
use std::cell::Cell;
use std::rc::Rc;

pub(crate) const DECODE_OUTPUT_LIMIT_PREFIX: &str = "decoded output exceeds configured limit of";

/// Bounded `/DecodeParms` view: everything `StreamFilter::set_decode_params`
/// needs, with no `Object` or `ObjectHandle` left in it.
///
/// `Absent` covers both a missing key and an explicit null, matching
/// `QPDF_Stream::filterable`'s treatment of a null `/DecodeParms` and
/// `SF_FlateLzwDecode::setDecodeParms`'s sole early return
/// (`SF_FlateLzwDecode.cc:24-26`).
/// `Present` carries the dictionary's entries in iteration order; a present
/// non-dictionary yields `Present` with no entries, which is what qpdf sees:
/// `setDecodeParms` asks `QPDFObjectHandle::getKeys`
/// (`QPDFObjectHandle.cc:997-1009`) for every non-null object, and it is
/// *`getKeys`* — not `setDecodeParms` — that warns
/// `typeWarning("dictionary", "treating as empty")` (`:1005`) and hands back
/// an empty key set.
#[derive(Debug, PartialEq)]
pub(crate) enum DecodeParams {
    Absent,
    Present(Vec<(Vec<u8>, ParamValue)>),
}

/// A `/DecodeParms` value reduced to the bounded scalars any filter reads.
///
/// `Int` carries `getIntValueAsInt`'s clamp, which saturates at *both* ends
/// (`QPDFObjectHandle.cc:526-543`). `Name` exists for `Crypt`'s `/Name`, which
/// selects the crypt filter — carrying it now keeps Phase 3's AES/Crypt cutover
/// from having to widen this shared type. `Other` is every remaining shape,
/// which every current filter rejects the same way `clamped_int_param`
/// rejected a non-integer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParamValue {
    Int(i32),
    Name(Vec<u8>),
    Other,
}

impl DecodeParams {
    pub(crate) fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    pub(crate) fn entries(&self) -> &[(Vec<u8>, ParamValue)] {
        match self {
            Self::Absent => &[],
            Self::Present(entries) => entries,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct FilterSpec {
    pub(crate) name: Vec<u8>,
    pub(crate) decode_params: DecodeParams,
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

pub(crate) fn decode_filter_specs_from_object(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
) -> Result<Vec<FilterSpec>> {
    let names: Vec<&[u8]> = match filter {
        None | Some(Object::Null) => return Ok(Vec::new()),
        Some(Object::Name(name)) => vec![name],
        Some(Object::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_name().ok_or_else(|| {
                    Error::Unsupported("stream filter type is not name or array".to_string())
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(Error::Unsupported(
                "stream filter type is not name or array".to_string(),
            ))
        }
    };

    if names.is_empty() {
        return Ok(Vec::new());
    }

    let params = match decode_params {
        None | Some(Object::Null) => vec![None; names.len()],
        Some(Object::Array(items)) if items.is_empty() => vec![None; names.len()],
        Some(Object::Array(items)) => {
            if items.len() != names.len() {
                return Err(Error::Unsupported(
                    "stream /DecodeParms length is inconsistent with filters".to_string(),
                ));
            }
            items
                .iter()
                .map(|item| (!matches!(item, Object::Null)).then_some(item))
                .collect()
        }
        Some(item) => vec![Some(item); names.len()],
    };

    Ok(names
        .into_iter()
        .zip(params)
        .map(|(name, decode_params)| FilterSpec {
            name: name.to_vec(),
            decode_params: decode_params_from_object(decode_params),
        })
        .collect())
}

fn decode_params_from_object(params: Option<&Object>) -> DecodeParams {
    match params {
        None | Some(Object::Null) => DecodeParams::Absent,
        Some(object) => DecodeParams::Present(match object.as_dict() {
            Some(dict) => dict
                .iter()
                .map(|(key, value)| (key.to_vec(), param_value_from_object(value)))
                .collect(),
            None => Vec::new(),
        }),
    }
}

fn param_value_from_object(value: &Object) -> ParamValue {
    match clamped_int_param(value) {
        Some(int) => ParamValue::Int(int),
        None => match value.as_name() {
            Some(name) => ParamValue::Name(name.to_vec()),
            None => ParamValue::Other,
        },
    }
}

/// Re-materialize the `Object` shape the Crypt stage provider still takes,
/// bridging the shape-neutral `FilterSpec` to the one caller that has not been
/// migrated.
///
/// Every `StreamFilter` now reads `&DecodeParams` directly, so the sole
/// remaining caller is the `PreparedStage::Crypt` arm of
/// `decode_stream_data_with_filters_and_crypt`; moving that provider closure
/// onto `&DecodeParams` retires this function entirely.
///
/// The round trip loses information in two places, both of which now reach
/// only that closure.
///
/// 1. A present *non-dictionary* reduces to `Present(vec![])` and comes back
///    as an empty dictionary, so a provider cannot tell the two apart. For the
///    codec filters that merge was a convergence toward qpdf — see
///    `FlateLzwStreamFilter::set_decode_params` below — but a crypt provider
///    reading its own shapes would have to reckon with it.
/// 2. `ParamValue::Other` flattens to `Object::Null`, so a reconstruction
///    cannot report which non-integer shape the source held. This is
///    unobservable today only because the sole production provider ignores its
///    argument and returns `Unsupported`. `ParamValue::Name` does survive the
///    round trip, so plan decision D2 (a Crypt provider reading `/Name`) is
///    unaffected.
pub(crate) fn decode_params_to_object(params: &DecodeParams) -> Option<Object> {
    if params.is_absent() {
        return None;
    }
    let mut dictionary = Dictionary::new();
    for (key, value) in params.entries() {
        dictionary.insert(key, param_value_to_object(value));
    }
    Some(Object::Dictionary(dictionary))
}

fn param_value_to_object(value: &ParamValue) -> Object {
    match value {
        ParamValue::Int(int) => Object::Integer(i64::from(*int)),
        ParamValue::Name(name) => Object::Name(name.clone()),
        // `Other` is the shape `param_value_from_object` reaches only after
        // both `clamped_int_param` and `as_name` decline, so the simplest
        // non-integer, non-name object stands in for every remaining shape.
        // This holds only for a value *inside* the parameter dictionary: at
        // the top level `Null` means absent (`DecodeParams::Absent`), so
        // reusing this mapping for a whole params object would invert the
        // meaning of `Other`.
        ParamValue::Other => Object::Null,
    }
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

impl FilterDecodeOutcome {
    #[cfg(test)]
    fn complete(data: Vec<u8>) -> Self {
        let cleanup_data_start = data.len();
        Self {
            data,
            cleanup_data_start,
            error: None,
        }
    }

    #[cfg(test)]
    fn into_strict_result(self) -> Result<Vec<u8>> {
        match self.error {
            Some(error) => Err(error.error),
            None => Ok(self.data),
        }
    }
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

#[cfg(test)]
pub(crate) fn ignore_warning(
    _: &str,
    _: i32,
    _: usize,
    _: FilterDecodePhase,
) -> PipelineResult<()> {
    Ok(())
}

#[cfg(test)]
fn ignore_codec_warning(_: &str, _: i32) -> PipelineResult<()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static EXPECTED_FIRST_INPUT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn expect_first_filter_input(data: &[u8]) {
    EXPECTED_FIRST_INPUT.set(data.as_ptr() as usize);
}

/// Rust equivalent of qpdf's `QPDFStreamFilter` extension boundary.
///
/// `pipe_decode` owns construction and completion of the filter's decode
/// pipeline. A whole-buffer result keeps flpdf's public API stable while the
/// individual codecs are migrated to incremental `Pipeline` stages.
pub(crate) trait StreamFilter {
    fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
        decode_params.is_absent()
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

    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome>;

    #[cfg(test)]
    fn pipe_decode(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<Vec<u8>> {
        self.pipe_decode_recovering(data, max_output, warn)?
            .into_strict_result()
    }

    // flpdf's current public decode API always requests full decoding, so
    // classification becomes a production decision only when decode levels
    // are introduced. Keep the qpdf extension contract available to later
    // registered filters.
    #[allow(dead_code)]
    fn is_specialized_compression(&self) -> bool {
        false
    }

    #[allow(dead_code)]
    fn is_lossy_compression(&self) -> bool {
        false
    }
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
        }
    }
}

/// Reduce a `/DecodeParms` value to an integer the way qpdf's
/// `getIntValueAsInt` does.
///
/// Values outside the 32-bit range saturate rather than failing, so a
/// `/Columns` far beyond `INT_MAX` behaves as `INT_MAX` does. The filters no
/// longer call this: it runs once per value in `param_value_from_object`, so
/// the clamp is applied while the `Object` shape is read and every filter sees
/// only the already-clamped `ParamValue::Int`.
fn clamped_int_param(value: &Object) -> Option<i32> {
    value
        .as_integer()
        .map(|value| value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
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
    fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
        // The one early return SF_FlateLzwDecode::setDecodeParms has
        // (SF_FlateLzwDecode.cc:24-26), for a null /DecodeParms. Every other
        // shape walks the keys and then falls through to the trailing check at
        // :68-70, which a present-but-empty parameter set reaches.
        if decode_params.is_absent() {
            return true;
        }

        let mut filterable = true;
        for (key, value) in decode_params.entries() {
            let key = key.as_slice();
            match key {
                b"Predictor" => match *value {
                    ParamValue::Int(predictor) => {
                        self.predictor = predictor;
                        if !((predictor == 1) || (predictor == 2) || (10..=15).contains(&predictor))
                        {
                            filterable = false;
                        }
                    }
                    ParamValue::Name(_) | ParamValue::Other => filterable = false,
                },
                b"Columns" | b"Colors" | b"BitsPerComponent" => match *value {
                    // qpdf stores these without range validation and defers
                    // rejection to pipeline construction.
                    ParamValue::Int(parameter) => match key {
                        b"Columns" => self.columns = parameter,
                        b"Colors" => self.colors = parameter,
                        _ => self.bits_per_component = parameter,
                    },
                    ParamValue::Name(_) | ParamValue::Other => filterable = false,
                },
                // qpdf consults /EarlyChange only for LZW streams.
                b"EarlyChange" if self.lzw => match *value {
                    ParamValue::Int(early_change) => {
                        self.early_code_change = early_change == 1;
                        if !((early_change == 0) || (early_change == 1)) {
                            filterable = false;
                        }
                    }
                    ParamValue::Name(_) | ParamValue::Other => filterable = false,
                },
                _ => {}
            }
        }

        if (self.predictor > 1) && (self.columns == 0) {
            filterable = false;
        }

        filterable
    }

    fn preflight_decode_pipeline(&self) -> Result<()> {
        if let Some((columns, colors, bits_per_component)) = self.decode_predictor_geometry()? {
            let mut sink = OutputBuffer::new(None);
            PngFilter::new(
                "png decode",
                &mut sink,
                PngFilterAction::Decode,
                columns,
                colors,
                bits_per_component,
            )
            .map_err(map_pipeline_error)?;
        }
        Ok(())
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
            Some((columns, colors, bits_per_component)) => {
                let mut predictor = PngFilter::new(
                    "png decode",
                    &mut sink,
                    PngFilterAction::Decode,
                    columns,
                    colors,
                    bits_per_component,
                )
                .map_err(map_pipeline_error)?;
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
    fn decode_predictor_geometry(&self) -> Result<Option<(u32, u32, u32)>> {
        if (10..=15).contains(&self.predictor) {
            return Ok(Some((
                to_uint(self.columns)?,
                to_uint(self.colors)?,
                to_uint(self.bits_per_component)?,
            )));
        }
        if self.predictor == 2 {
            // Declared deviation: qpdf builds Pl_TIFFPredictor here. flpdf has
            // no TIFF predictor component yet and reports the restriction at
            // qpdf's construction point.
            return Err(Error::Unsupported(
                "/DecodeParms /Predictor 2 is not supported for this stream type".to_string(),
            ));
        }
        Ok(None)
    }

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

fn filter_decode_phase(finish_phase: Option<&Cell<bool>>) -> FilterDecodePhase {
    if finish_phase.is_some_and(Cell::get) {
        FilterDecodePhase::Finish
    } else {
        FilterDecodePhase::Write
    }
}

struct Ascii85StreamFilter;

impl StreamFilter for Ascii85StreamFilter {
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

#[cfg(test)]
struct TestStreamFilter;

#[cfg(test)]
impl StreamFilter for TestStreamFilter {
    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Ok(FilterDecodeOutcome::complete(data.to_vec()))
    }
}

#[cfg(test)]
struct BorrowedInputProbe;

#[cfg(test)]
impl StreamFilter for BorrowedInputProbe {
    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        EXPECTED_FIRST_INPUT.with(|expected| {
            assert_eq!(data.as_ptr() as usize, expected.get());
        });
        Ok(FilterDecodeOutcome::complete(data.to_vec()))
    }
}

#[cfg(test)]
struct PostPreflightFailure;

#[cfg(test)]
impl StreamFilter for PostPreflightFailure {
    fn pipe_decode_recovering(
        &mut self,
        _: &[u8],
        _: Option<usize>,
        _: &mut dyn FnMut(&str, i32, usize, FilterDecodePhase) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        Err(Error::Internal(
            "test post-preflight decode failure".to_string(),
        ))
    }
}

pub(crate) fn stream_filter_for(filter_name: &[u8]) -> Option<Box<dyn StreamFilter>> {
    match filter_name {
        b"FlateDecode" => Some(Box::new(FlateLzwStreamFilter::new(false))),
        b"LZWDecode" => Some(Box::new(FlateLzwStreamFilter::new(true))),
        b"ASCII85Decode" => Some(Box::new(Ascii85StreamFilter)),
        b"ASCIIHexDecode" => Some(Box::new(AsciiHexStreamFilter)),
        b"RunLengthDecode" => Some(Box::new(RunLengthStreamFilter)),
        #[cfg(test)]
        b"TestRejectDecode" => Some(Box::new(TestStreamFilter)),
        #[cfg(test)]
        b"TestBorrowedInput" => Some(Box::new(BorrowedInputProbe)),
        #[cfg(test)]
        b"TestPostPreflightFailure" => Some(Box::new(PostPreflightFailure)),
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

#[cfg(test)]
fn decode_flate_chunks<'a>(
    chunks: impl IntoIterator<Item = &'a [u8]>,
    max_output: Option<usize>,
    warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
) -> Result<Vec<u8>> {
    let mut sink = OutputBuffer::new(max_output);
    {
        let mut flate = Flate::new(
            "stream inflate",
            &mut sink,
            FlateAction::Inflate,
            DEFAULT_OUT_BUFFER_SIZE,
        )
        .map_err(map_pipeline_error)?;
        flate.set_warn_callback(|message, code| warn(message, code));
        for chunk in chunks {
            flate.write(chunk).map_err(map_pipeline_error)?;
        }
        flate.finish().map_err(map_pipeline_error)?;
    }
    Ok(sink.data)
}

#[cfg(test)]
fn decode_flate(data: &[u8], max_output: Option<usize>) -> Result<Vec<u8>> {
    decode_flate_chunks([data], max_output, &mut ignore_codec_warning)
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

/// Resolve the PNG predictor geometry a writer must apply for `/DecodeParms`.
///
/// Returns `Ok(None)` when the parameters select no PNG predictor. The
/// parameters are validated through the same `SF_FlateLzwDecode` state the
/// decode path uses, so both directions accept exactly the same dictionaries.
pub(crate) fn png_encode_geometry(
    filter_name: &[u8],
    decode_params: &DecodeParams,
) -> Result<Option<(u32, u32, u32)>> {
    let mut filter = FlateLzwStreamFilter::new(filter_name == b"LZWDecode");
    if !filter.set_decode_params(decode_params) {
        return Err(Error::Unsupported(format!(
            "stream filter {} does not support supplied /DecodeParms",
            String::from_utf8_lossy(filter_name)
        )));
    }
    filter.decode_predictor_geometry()
}

/// Apply the PNG predictor to unencoded stream data.
///
/// qpdf's `Pl_PNGFilter` encoder always emits the Up filter, so the predictor
/// number selects only whether the predictor runs, never which row filter the
/// output uses.
pub(crate) fn encode_png_predictor(
    data: &[u8],
    columns: u32,
    colors: u32,
    bits_per_component: u32,
) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut stage = PngFilter::new(
            "png encode",
            &mut sink,
            PngFilterAction::Encode,
            columns,
            colors,
            bits_per_component,
        )
        .map_err(map_pipeline_error)?;
        stage.write(data).map_err(map_pipeline_error)?;
        stage.finish().map_err(map_pipeline_error)?;
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
    use super::{
        decode_filter_specs_from_object, decode_flate, decode_flate_chunks,
        decode_params_from_object, encode_flate, encode_run_length, ignore_codec_warning,
        ignore_warning, normalize_filter_name, stream_filter_for, Ascii85StreamFilter,
        DecodeParams, FlateLzwStreamFilter, OutputBuffer, ParamValue, Pipeline, StreamFilter,
        DECODE_OUTPUT_LIMIT_PREFIX,
    };
    use crate::pipeline::lzw::pack_codes;
    use crate::{Dictionary, Error, Object};
    use std::cell::{Cell, RefCell};

    #[test]
    fn run_length_encoder_uses_qpdf_two_byte_run() {
        assert_eq!(encode_run_length(b"AA").unwrap(), [0xff, b'A', 0x80]);
    }

    #[test]
    fn scalar_decode_parms_are_reused_for_each_filter() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let decode_parms = params(&[("Columns", Object::Integer(7))]);

        let specs = decode_filter_specs_from_object(Some(&filter), Some(&decode_parms)).unwrap();

        let replicated = DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(7))]);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].decode_params, replicated);
        assert_eq!(specs[1].decode_params, replicated);
    }

    #[test]
    fn decode_parms_array_must_align_with_filter_array() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let params = Object::Array(vec![Object::Null]);

        let error = decode_filter_specs_from_object(Some(&filter), Some(&params)).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
        );
    }

    #[test]
    fn empty_decode_parms_array_is_null_and_filter_abbreviation_expands() {
        let filter = Object::Name(b"Fl".to_vec());
        let params = Object::Array(Vec::new());

        let specs = decode_filter_specs_from_object(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs[0].normalized_name(), b"FlateDecode");
        assert!(specs[0].decode_params.is_absent());
    }

    #[test]
    fn no_filter_ignores_decode_parms() {
        let params = Object::Array(vec![Object::Integer(1)]);

        let specs = decode_filter_specs_from_object(None, Some(&params)).unwrap();

        assert!(specs.is_empty());
    }

    #[test]
    fn non_name_filter_item_is_rejected_before_decode() {
        let filter = Object::Array(vec![Object::Integer(1)]);

        let error = decode_filter_specs_from_object(Some(&filter), None).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn scalar_non_name_filter_is_rejected_before_decode() {
        let error = decode_filter_specs_from_object(Some(&Object::Integer(1)), None).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn empty_filter_array_ignores_decode_parms() {
        let filter = Object::Array(Vec::new());
        let params = Object::Array(vec![Object::Integer(1)]);

        assert!(
            decode_filter_specs_from_object(Some(&filter), Some(&params))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn object_shape_reader_distinguishes_absent_null_and_present_non_dictionary() {
        let name = Object::Name(b"FlateDecode".to_vec());

        let absent = decode_filter_specs_from_object(Some(&name), None).unwrap();
        assert!(matches!(absent[0].decode_params, DecodeParams::Absent));
        assert!(absent[0].decode_params.entries().is_empty());

        let null = decode_filter_specs_from_object(Some(&name), Some(&Object::Null)).unwrap();
        assert!(matches!(null[0].decode_params, DecodeParams::Absent));

        let scalar = Object::Integer(1);
        let present = decode_filter_specs_from_object(Some(&name), Some(&scalar)).unwrap();
        // qpdf's SF_FlateLzwDecode treats a non-dictionary as an empty
        // dictionary, but the default StreamFilter::set_decode_params rejects
        // any non-null.
        assert!(matches!(present[0].decode_params, DecodeParams::Present(_)));
        assert!(present[0].decode_params.entries().is_empty());
    }

    #[test]
    fn object_shape_reader_reduces_each_parameter_value_to_its_bounded_shape() {
        let name = Object::Name(b"FlateDecode".to_vec());
        let dictionary = params(&[
            // getIntValueAsInt saturates at both ends, so pin both.
            ("Colors", Object::Integer(i64::from(i32::MIN) - 10)),
            ("Columns", Object::Integer(i64::from(i32::MAX) + 10)),
            ("Name", Object::Name(b"Identity".to_vec())),
            ("Whatever", Object::Null),
        ]);

        let specs = decode_filter_specs_from_object(Some(&name), Some(&dictionary)).unwrap();

        assert_eq!(
            specs[0].decode_params.entries().to_vec(),
            vec![
                (b"Colors".to_vec(), ParamValue::Int(i32::MIN)),
                (b"Columns".to_vec(), ParamValue::Int(i32::MAX)),
                (b"Name".to_vec(), ParamValue::Name(b"Identity".to_vec())),
                (b"Whatever".to_vec(), ParamValue::Other),
            ]
        );
    }

    #[test]
    fn output_buffer_has_qpdf_pipeline_identifier() {
        assert_eq!(OutputBuffer::new(None).identifier(), "stream data buffer");
    }

    #[test]
    fn one_element_decode_parms_array_aligns_with_name_filter() {
        let filter = Object::Name(b"FlateDecode".to_vec());
        let decode_parms = Object::Array(vec![params(&[("Columns", Object::Integer(7))])]);

        let specs = decode_filter_specs_from_object(Some(&filter), Some(&decode_parms)).unwrap();

        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].decode_params,
            DecodeParams::Present(vec![(b"Columns".to_vec(), ParamValue::Int(7))])
        );
    }

    #[test]
    fn qpdf_filter_abbreviations_expand_without_changing_full_names() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"Fl", b"FlateDecode"),
            (b"LZW", b"LZWDecode"),
            (b"A85", b"ASCII85Decode"),
            (b"AHx", b"ASCIIHexDecode"),
            (b"RL", b"RunLengthDecode"),
            (b"CCF", b"CCITTFaxDecode"),
            (b"DCT", b"DCTDecode"),
            (b"FlateDecode", b"FlateDecode"),
        ];

        for &(abbreviation, expected) in cases {
            let filter = Object::Name(abbreviation.to_vec());
            let specs = decode_filter_specs_from_object(Some(&filter), None).unwrap();
            assert_eq!(specs[0].normalized_name(), expected);
        }
    }

    #[test]
    fn flate_decode_is_invariant_across_input_chunks() {
        let encoded = encode_flate(b"chunk boundaries must not matter").unwrap();
        let whole = decode_flate_chunks([encoded.as_slice()], None, &mut |_, _| Ok(())).unwrap();
        let split = decode_flate_chunks(encoded.chunks(1), None, &mut |_, _| Ok(())).unwrap();

        assert_eq!(whole, b"chunk boundaries must not matter");
        assert_eq!(split, whole);
    }

    #[test]
    fn flate_limit_rejects_one_byte_over_but_accepts_exact_boundary() {
        let encoded = encode_flate(&vec![b'A'; 2_000]).unwrap();

        let error = decode_flate(&encoded, Some(1_999)).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            format!(
                "unsupported PDF feature: {DECODE_OUTPUT_LIMIT_PREFIX} {} bytes",
                1_999
            )
        );
        assert_eq!(decode_flate(&encoded, Some(2_000)).unwrap().len(), 2_000);
    }

    #[test]
    fn incomplete_input_reports_qpdf_warning_before_downstream_finish() {
        let warnings = RefCell::new(Vec::new());

        let decoded = decode_flate_chunks([b"\x78".as_slice()], None, &mut |message, code| {
            warnings.borrow_mut().push((message.to_string(), code));
            Ok(())
        })
        .unwrap();

        assert!(decoded.is_empty());
        assert_eq!(
            warnings.into_inner(),
            vec![(
                "input stream is complete but output may still be valid".to_string(),
                -5,
            )]
        );
        assert!(
            decode_flate_chunks([b"\x78".as_slice()], None, &mut ignore_codec_warning)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_inflate_skips_codec_and_warning_like_qpdf() {
        let decoded =
            decode_flate_chunks(std::iter::empty(), None, &mut ignore_codec_warning).unwrap();

        assert!(decoded.is_empty());
    }

    #[test]
    fn malformed_flate_header_retains_qpdf_pipeline_identifier_and_timing() {
        let error = decode_flate_chunks(
            [b"\x78\x00".as_slice(), b"not reached".as_slice()],
            None,
            &mut ignore_codec_warning,
        )
        .unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream inflate: inflate: data: incorrect header check"
        );
    }

    #[test]
    fn empty_encode_skips_codec_and_emits_no_wrapper_like_qpdf() {
        assert!(encode_flate(b"").unwrap().is_empty());
    }

    #[test]
    fn flate_factory_exposes_qpdf_stream_filter_contract() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");

        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(!filter.is_specialized_compression());
        assert!(!filter.is_lossy_compression());

        let encoded = encode_flate(b"factory pipeline").unwrap();
        let decoded = filter
            .pipe_decode(&encoded, None, &mut |_, _, _, _| Ok(()))
            .unwrap();
        assert_eq!(decoded, b"factory pipeline");
    }

    /// Crosses the shape seam deliberately: a present non-dictionary is only
    /// visible as an `Object`, and the property under test is that reading it
    /// leaves the filter filterable the way `getKeys`' empty key set does.
    #[test]
    fn flate_factory_treats_non_dictionary_decode_params_as_empty_like_qpdf() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");

        assert!(filter.set_decode_params(&decode_params_from_object(Some(&Object::Integer(1)))));
    }

    #[test]
    fn flate_lzw_filter_retains_only_the_qpdf_parameter_set() {
        // The adapter keeps the five scalar parameters qpdf keeps; an
        // arbitrarily large source object contributes none of them.
        let params = Object::String(vec![b'x'; 64 * 1024]);
        let mut filter = FlateLzwStreamFilter::new(false);

        assert!(filter.set_decode_params(&decode_params_from_object(Some(&params))));
        assert_eq!(
            (
                filter.predictor,
                filter.columns,
                filter.colors,
                filter.bits_per_component,
                filter.early_code_change,
            ),
            (1, 1, 1, 8, true),
            "a non-dictionary parameter object leaves every default in place"
        );
    }

    #[test]
    fn factory_returns_all_production_stream_filters() {
        for name in [
            b"FlateDecode".as_slice(),
            b"LZWDecode",
            b"ASCII85Decode",
            b"ASCIIHexDecode",
            b"RunLengthDecode",
        ] {
            assert!(stream_filter_for(name).is_some(), "{name:?}");
        }
    }

    fn params(entries: &[(&str, Object)]) -> Object {
        let mut dictionary = Dictionary::new();
        for (key, value) in entries {
            dictionary.insert(*key, value.clone());
        }
        Object::Dictionary(dictionary)
    }

    /// Build the `/DecodeParms` shape `StreamFilter::set_decode_params` reads.
    ///
    /// The filters no longer see an `Object`, so a filter test states the
    /// bounded values directly; the `Object` -> `ParamValue` reduction is
    /// `param_value_from_object`'s contract and is pinned by
    /// `object_shape_reader_reduces_each_parameter_value_to_its_bounded_shape`.
    fn neutral_params(entries: &[(&str, ParamValue)]) -> DecodeParams {
        DecodeParams::Present(
            entries
                .iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value.clone()))
                .collect(),
        )
    }

    fn accepts(lzw: bool, entries: &[(&str, ParamValue)]) -> bool {
        FlateLzwStreamFilter::new(lzw).set_decode_params(&neutral_params(entries))
    }

    #[test]
    fn flate_lzw_filter_accepts_absent_and_null_decode_params() {
        let mut filter = FlateLzwStreamFilter::new(true);
        // A missing key and an explicit null both reduce to `Absent`.
        assert!(filter.set_decode_params(&decode_params_from_object(None)));
        assert!(filter.set_decode_params(&decode_params_from_object(Some(&Object::Null))));
        assert!(!filter.is_specialized_compression());
        assert!(!filter.is_lossy_compression());
    }

    #[test]
    fn flate_filter_reads_predictor_geometry_from_neutral_params() {
        let mut filter = FlateLzwStreamFilter::new(false);
        let params = DecodeParams::Present(vec![
            (b"Predictor".to_vec(), ParamValue::Int(12)),
            (b"Columns".to_vec(), ParamValue::Int(4)),
        ]);
        assert!(filter.set_decode_params(&params));
        assert_eq!(filter.predictor, 12);
        assert_eq!(filter.columns, 4);
    }

    #[test]
    fn non_null_params_still_make_a_parameterless_filter_unfilterable() {
        let mut filter = Ascii85StreamFilter;
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
    }

    #[test]
    fn a_fresh_flate_filter_accepts_both_present_shapes_the_neutral_form_merges() {
        // The neutral form collapses "present non-dictionary" and "present empty
        // dictionary" into `Present(vec![])`, removing flpdf's own early
        // `return true` for a non-dictionary. That shortcut was never qpdf's:
        // `SF_FlateLzwDecode::setDecodeParms` (`libqpdf/SF_FlateLzwDecode.cc:21-73`)
        // early-returns only for `isNull()` (`:24-26`); a present non-dictionary
        // reaches `getKeys()`, which warns `typeWarning("dictionary", "treating as
        // empty")` (`libqpdf/QPDFObjectHandle.cc:997-1009`, warning at `:1005`),
        // yields an empty set, and falls through to the trailing
        // `(predictor > 1) && (columns == 0)` check at `:68-70`. So this merge is a
        // CONVERGENCE toward qpdf, not a tolerated loss.
        //
        // Both shapes still answer `true` because every caller applies params to a
        // freshly constructed adapter (defaults `predictor = 1, columns = 1`),
        // making that trailing check false either way. This assertion is what fails
        // if an adapter is ever reused across specs.
        assert!(
            FlateLzwStreamFilter::new(false).set_decode_params(&DecodeParams::Present(Vec::new()))
        );
        assert!(
            FlateLzwStreamFilter::new(true).set_decode_params(&DecodeParams::Present(Vec::new()))
        );
    }

    #[test]
    fn predictor_values_outside_the_supported_set_are_not_filterable() {
        for predictor in [1, 2, 10, 11, 12, 13, 14, 15] {
            assert!(
                accepts(false, &[("Predictor", ParamValue::Int(predictor))]),
                "predictor {predictor}"
            );
        }
        for predictor in [-1, 0, 3, 9, 16, 100] {
            assert!(
                !accepts(false, &[("Predictor", ParamValue::Int(predictor))]),
                "predictor {predictor}"
            );
        }
        assert!(!accepts(
            false,
            &[("Predictor", ParamValue::Name(b"12".to_vec()))]
        ));
    }

    #[test]
    fn a_predictor_above_one_requires_a_nonzero_columns_value() {
        assert!(!accepts(
            false,
            &[
                ("Predictor", ParamValue::Int(12)),
                ("Columns", ParamValue::Int(0)),
            ]
        ));
        assert!(accepts(
            false,
            &[
                ("Predictor", ParamValue::Int(1)),
                ("Columns", ParamValue::Int(0)),
            ]
        ));
        assert!(accepts(
            false,
            &[
                ("Predictor", ParamValue::Int(12)),
                ("Columns", ParamValue::Int(4)),
            ]
        ));
    }

    #[test]
    fn geometry_parameters_are_retained_without_range_validation() {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(-4)),
            ("Colors", ParamValue::Int(-1)),
            ("BitsPerComponent", ParamValue::Int(99)),
        ])));
        assert_eq!(
            (filter.columns, filter.colors, filter.bits_per_component),
            (-4, -1, 99)
        );
        assert!(!accepts(false, &[("Columns", ParamValue::Other)]));
        assert!(!accepts(false, &[("Colors", ParamValue::Other)]));
        assert!(!accepts(false, &[("BitsPerComponent", ParamValue::Other)]));
    }

    /// Crosses the shape seam deliberately: the clamp now happens while the
    /// `Object` is read, so only an out-of-range `Object::Integer` can show
    /// that a `/Columns` beyond `INT_MAX` still reaches the filter as
    /// `INT_MAX`.
    #[test]
    fn integer_parameters_saturate_at_the_32_bit_boundary() {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(
            filter.set_decode_params(&decode_params_from_object(Some(&params(&[
                ("Predictor", Object::Integer(12)),
                ("Columns", Object::Integer(i64::from(i32::MAX) + 10)),
                ("Colors", Object::Integer(i64::from(i32::MIN) - 10)),
            ]))))
        );
        assert_eq!((filter.columns, filter.colors), (i32::MAX, i32::MIN));
    }

    #[test]
    fn early_change_is_read_only_for_lzw_streams() {
        let mut lzw = FlateLzwStreamFilter::new(true);
        assert!(lzw.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(0))])));
        assert!(!lzw.early_code_change);

        let mut lzw = FlateLzwStreamFilter::new(true);
        assert!(lzw.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(1))])));
        assert!(lzw.early_code_change);

        // A value outside {0, 1} makes an LZW stream unfilterable.
        assert!(!accepts(true, &[("EarlyChange", ParamValue::Int(7))]));
        assert!(!accepts(
            true,
            &[("EarlyChange", ParamValue::Name(b"1".to_vec()))]
        ));

        // The same parameters are ignored entirely on a Flate stream.
        let mut flate = FlateLzwStreamFilter::new(false);
        assert!(flate.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(7))])));
        assert!(flate.early_code_change);
    }

    #[test]
    fn unrecognized_decode_params_keys_are_ignored() {
        assert!(accepts(
            true,
            &[("Whatever", ParamValue::Name(b"x".to_vec()))]
        ));
    }

    #[test]
    fn lzw_streams_decode_through_the_registered_filter() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
        let mut filter = stream_filter_for(b"LZWDecode").expect("registered LZW filter");

        let decoded = filter
            .pipe_decode(&[0x80, 0x10, 0x60, 0x20], None, &mut ignore_warning)
            .expect("LZW decode");

        assert_eq!(decoded, b"A");
    }

    #[test]
    fn lzw_early_change_zero_changes_the_decoded_bytes() {
        let stream: &[u8] = &[0x80, 0x10, 0x48, 0x50, 0x28, 0x24, 0x0e, 0x0d, 0x01];
        let mut filter = stream_filter_for(b"LZWDecode").expect("registered LZW filter");
        assert!(filter.set_decode_params(&neutral_params(&[("EarlyChange", ParamValue::Int(0))])));

        let decoded = filter
            .pipe_decode(stream, None, &mut ignore_warning)
            .expect("LZW decode");

        assert_eq!(decoded, b"ABABABABABAB");
    }

    #[test]
    fn abbreviated_filter_names_reach_the_flate_and_lzw_filters() {
        for name in [b"Fl".as_slice(), b"LZW"] {
            assert!(
                stream_filter_for(normalize_filter_name(name)).is_some(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn tiff_predictor_is_reported_at_pipeline_construction() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(2)),
            ("Columns", ParamValue::Int(4)),
        ])));

        let error = filter
            .pipe_decode(b"", None, &mut ignore_warning)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: /DecodeParms /Predictor 2 is not supported for this stream type"
        );
    }

    #[test]
    fn negative_geometry_is_rejected_when_the_predictor_pipeline_is_built() {
        for (key, value) in [("Columns", -4), ("Colors", -1), ("BitsPerComponent", -8)] {
            let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
            assert!(filter.set_decode_params(&neutral_params(&[
                ("Predictor", ParamValue::Int(12)),
                ("Columns", ParamValue::Int(4)),
                (key, ParamValue::Int(value)),
            ])));

            let error = filter
                .pipe_decode(b"", None, &mut ignore_warning)
                .unwrap_err();

            assert_eq!(
                error.to_string(),
                format!(
                    "unsupported PDF feature: integer out of range converting {value} \
                     from a 4-byte signed type to a 4-byte unsigned type"
                ),
                "{key}"
            );
        }
    }

    #[test]
    fn invalid_predictor_geometry_is_reported_before_any_codec_write() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
            ("BitsPerComponent", ParamValue::Int(3)),
        ])));

        // The input is not valid deflate data, so reaching the codec at all
        // would produce a different error.
        let error = filter
            .pipe_decode(b"not deflate", None, &mut ignore_warning)
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: PNGFilter created with invalid bits_per_sample \
             not 1, 2, 4, 8, or 16"
        );
    }

    #[test]
    fn predictor_decoding_runs_after_the_codec_in_one_chain() {
        let rows: &[u8] = &[2, 0x01, 0x02, 0x03, 0x04, 2, 0x01, 0x01, 0x01, 0x01];
        let encoded = encode_flate(rows).expect("flate encode");
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
        ])));

        let decoded = filter
            .pipe_decode(&encoded, None, &mut ignore_warning)
            .expect("predicted flate decode");

        assert_eq!(
            decoded,
            vec![0x01, 0x02, 0x03, 0x04, 0x02, 0x03, 0x04, 0x05]
        );
    }

    #[test]
    fn the_output_limit_applies_to_post_predictor_bytes() {
        let rows: &[u8] = &[2, 0x01, 0x02, 0x03, 0x04, 2, 0x01, 0x01, 0x01, 0x01];
        let encoded = encode_flate(rows).expect("flate encode");
        let predicted = neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(4)),
        ]);

        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&predicted));
        assert_eq!(
            filter
                .pipe_decode(&encoded, Some(8), &mut ignore_warning)
                .expect("eight decoded bytes fit the cap")
                .len(),
            8
        );

        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(&predicted));
        let error = filter
            .pipe_decode(&encoded, Some(7), &mut ignore_warning)
            .unwrap_err();
        assert!(
            error.to_string().contains(DECODE_OUTPUT_LIMIT_PREFIX),
            "{error}"
        );
    }

    #[test]
    fn lzw_decoding_honors_the_output_limit() {
        let mut filter = stream_filter_for(b"LZWDecode").expect("registered LZW filter");
        let error = filter
            .pipe_decode(&[0x80, 0x10, 0x60, 0x20], Some(0), &mut ignore_warning)
            .unwrap_err();

        assert!(
            error.to_string().contains(DECODE_OUTPUT_LIMIT_PREFIX),
            "{error}"
        );
    }

    #[test]
    fn flate_and_lzw_predictor_finish_output_keeps_its_cleanup_boundary() {
        let predictor = neutral_params(&[
            ("Predictor", ParamValue::Int(12)),
            ("Columns", ParamValue::Int(2)),
        ]);

        let mut flate = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(flate.set_decode_params(&predictor));
        let mut flate_payload = encode_flate(&[0, b'A']).expect("encode predicted bytes");
        flate_payload.truncate(flate_payload.len() - 4);
        let mut warnings = Vec::new();
        let flate_outcome = flate
            .pipe_decode_recovering(&flate_payload, Some(1), &mut |message, code, _, _| {
                warnings.push((message.to_string(), code));
                Ok(())
            })
            .expect("constructed Flate pipeline");
        assert_eq!(flate_outcome.data, b"A");
        assert_eq!(flate_outcome.cleanup_data_start, 0);
        assert!(
            !flate_outcome
                .error
                .expect("finish output hits limit")
                .during_write
        );
        assert_eq!(
            warnings,
            [(
                "input stream is complete but output may still be valid".to_string(),
                -5
            )]
        );

        let mut lzw = stream_filter_for(b"LZWDecode").expect("registered LZW filter");
        assert!(lzw.set_decode_params(&predictor));
        let lzw_outcome = lzw
            .pipe_decode_recovering(
                &pack_codes(&[256, 0, u32::from(b'A'), 257], true),
                Some(1),
                &mut ignore_warning,
            )
            .expect("constructed LZW pipeline");
        assert_eq!(lzw_outcome.data, b"A");
        assert_eq!(lzw_outcome.cleanup_data_start, 0);
        assert!(
            !lzw_outcome
                .error
                .expect("finish output hits limit")
                .during_write
        );
    }

    #[test]
    fn codec_warning_phase_distinguishes_write_from_finish() {
        let phase = Cell::new(false);
        assert_eq!(
            super::filter_decode_phase(None),
            super::FilterDecodePhase::Write
        );
        assert_eq!(
            super::filter_decode_phase(Some(&phase)),
            super::FilterDecodePhase::Write
        );
        phase.set(true);
        assert_eq!(
            super::filter_decode_phase(Some(&phase)),
            super::FilterDecodePhase::Finish
        );
    }

    #[test]
    fn ascii_and_run_length_factories_expose_qpdf_stream_filter_contract() {
        for (name, specialized) in [
            (b"ASCII85Decode".as_slice(), false),
            (b"ASCIIHexDecode".as_slice(), false),
            (b"RunLengthDecode".as_slice(), true),
        ] {
            let mut filter = stream_filter_for(name).expect("registered stream filter");

            assert!(filter.set_decode_params(&DecodeParams::Absent), "{name:?}");
            assert!(
                !filter.set_decode_params(&DecodeParams::Present(Vec::new())),
                "{name:?}"
            );
            assert_eq!(filter.is_specialized_compression(), specialized, "{name:?}");
            assert!(!filter.is_lossy_compression(), "{name:?}");
        }
    }

    #[test]
    fn ascii_and_run_length_factories_decode_through_pipelines() {
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            (b"ASCII85Decode", b"z~>", &[0, 0, 0, 0]),
            (b"ASCIIHexDecode", b"4142>", b"AB"),
            (b"RunLengthDecode", &[0xff, b'A', 0x80], b"AA"),
        ];

        for &(name, encoded, expected) in cases {
            let decoded = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, None, &mut ignore_warning)
                .unwrap();

            assert_eq!(decoded, expected, "{name:?}");
        }
    }

    #[test]
    fn ascii_and_run_length_factories_enforce_output_limit_boundaries() {
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            (b"ASCII85Decode", b"z~>", &[0, 0, 0, 0]),
            (b"ASCIIHexDecode", b"4142>", b"AB"),
            (b"RunLengthDecode", &[0xff, b'A', 0x80], b"AA"),
        ];

        for &(name, encoded, expected) in cases {
            let below = expected.len() - 1;
            let error = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, Some(below), &mut ignore_warning)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("unsupported PDF feature: {DECODE_OUTPUT_LIMIT_PREFIX} {below} bytes"),
                "{name:?}"
            );

            let decoded = stream_filter_for(name)
                .expect("registered stream filter")
                .pipe_decode(encoded, Some(expected.len()), &mut ignore_warning)
                .unwrap();
            assert_eq!(decoded, expected, "{name:?}");
        }
    }

    #[test]
    fn default_stream_filter_contract_accepts_only_null_params() {
        let mut filter = stream_filter_for(b"TestRejectDecode").expect("test filter");

        // `Absent` is how a missing key and an explicit null both arrive.
        assert!(filter.set_decode_params(&DecodeParams::Absent));
        assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
        assert_eq!(
            filter
                .pipe_decode(b"test filter", None, &mut ignore_warning)
                .unwrap(),
            b"test filter"
        );
    }
}
