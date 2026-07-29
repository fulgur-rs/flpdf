//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name, DecodeParms-alignment, and decode-pipeline construction responsibilities.

use crate::pipeline::ascii85::Ascii85Decoder;
use crate::pipeline::ascii_hex::AsciiHexDecoder;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE};
use crate::pipeline::lzw::LzwDecoder;
use crate::pipeline::png_filter::{PngFilter, PngFilterAction};
use crate::pipeline::run_length::{RunLength, RunLengthAction};
use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
use crate::{Error, Object, Result};
use std::cell::Cell;
use std::rc::Rc;

pub(crate) const DECODE_OUTPUT_LIMIT_PREFIX: &str = "decoded output exceeds configured limit of";

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterSpec<'a> {
    pub(crate) name: &'a [u8],
    pub(crate) decode_params: Option<&'a Object>,
}

impl FilterSpec<'_> {
    pub(crate) fn normalized_name(&self) -> &[u8] {
        normalize_filter_name(self.name)
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

pub(crate) fn decode_filter_specs<'a>(
    filter: Option<&'a Object>,
    decode_params: Option<&'a Object>,
) -> Result<Vec<FilterSpec<'a>>> {
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
}

impl OutputBuffer {
    fn new(max_output: Option<usize>) -> Self {
        Self {
            data: Vec::new(),
            max_output,
            cleanup_data_start: None,
            finish_phase: Rc::new(Cell::new(false)),
        }
    }

    fn finish_phase(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.finish_phase)
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
        if let Some(limit) = self.max_output {
            if data.len() > limit.saturating_sub(self.data.len()) {
                return Err(PipelineError::runtime(format!(
                    "{DECODE_OUTPUT_LIMIT_PREFIX} {limit} bytes"
                )));
            }
        }
        if self.finish_phase.get() && self.cleanup_data_start.is_none() {
            self.cleanup_data_start = Some(self.data.len());
        }
        self.data.extend_from_slice(data);
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
}

fn write_and_finish(
    stage: &mut dyn Pipeline,
    data: &[u8],
    finish_phase: Option<&Cell<bool>>,
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
            })
        }
        Err(error) => {
            if let Some(finish_phase) = finish_phase {
                finish_phase.set(true);
            }
            let _ = stage.finish();
            Some(StagePipelineError {
                error,
                during_write: true,
            })
        }
    }
}

fn map_stage_error(error: StagePipelineError) -> FilterDecodeError {
    FilterDecodeError {
        error: map_pipeline_error(error.error),
        during_write: error.during_write,
    }
}

#[cfg(test)]
pub(crate) fn ignore_warning(_: &str, _: i32) -> PipelineResult<()> {
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
    fn set_decode_params(&mut self, decode_params: Option<&Object>) -> bool {
        decode_params.is_none_or(|params| matches!(params, Object::Null))
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
        warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome>;

    #[cfg(test)]
    fn pipe_decode(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
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

/// Read an integer parameter the way qpdf's `getIntValueAsInt` does.
///
/// Values outside the 32-bit range saturate rather than failing, so a
/// `/Columns` far beyond `INT_MAX` behaves as `INT_MAX` does.
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
    fn set_decode_params(&mut self, decode_params: Option<&Object>) -> bool {
        let Some(params) = decode_params else {
            return true;
        };
        if matches!(params, Object::Null) {
            return true;
        }
        // SF_FlateLzwDecode::setDecodeParms asks getKeys() for every non-null
        // object. qpdf warns and treats a non-dictionary as an empty
        // dictionary, so it remains filterable.
        let Some(params) = params.as_dict() else {
            return true;
        };

        let mut filterable = true;
        for (key, value) in params.iter() {
            match key {
                b"Predictor" => match clamped_int_param(value) {
                    Some(predictor) => {
                        self.predictor = predictor;
                        if !((predictor == 1) || (predictor == 2) || (10..=15).contains(&predictor))
                        {
                            filterable = false;
                        }
                    }
                    None => filterable = false,
                },
                b"Columns" | b"Colors" | b"BitsPerComponent" => match clamped_int_param(value) {
                    // qpdf stores these without range validation and defers
                    // rejection to pipeline construction.
                    Some(parameter) => match key {
                        b"Columns" => self.columns = parameter,
                        b"Colors" => self.colors = parameter,
                        _ => self.bits_per_component = parameter,
                    },
                    None => filterable = false,
                },
                // qpdf consults /EarlyChange only for LZW streams.
                b"EarlyChange" if self.lzw => match clamped_int_param(value) {
                    Some(early_change) => {
                        self.early_code_change = early_change == 1;
                        if !((early_change == 0) || (early_change == 1)) {
                            filterable = false;
                        }
                    }
                    None => filterable = false,
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
        warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        let geometry = self.decode_predictor_geometry()?;
        let mut sink = OutputBuffer::new(max_output);
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
                self.pipe_codec(&mut predictor, data, warn)?
            }
            None => self.pipe_codec(&mut sink, data, warn)?,
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
        warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
    ) -> Result<Option<FilterDecodeError>> {
        let error = if self.lzw {
            let mut stage = LzwDecoder::new("lzw decode", next, self.early_code_change);
            write_and_finish(&mut stage, data, None)
        } else {
            let mut stage = Flate::new(
                "stream inflate",
                next,
                FlateAction::Inflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )
            .map_err(map_pipeline_error)?;
            stage.set_warn_callback(|message, code| warn(message, code));
            write_and_finish(&mut stage, data, None)
        };
        Ok(error.map(map_stage_error))
    }
}

struct Ascii85StreamFilter;

impl StreamFilter for Ascii85StreamFilter {
    fn pipe_decode_recovering(
        &mut self,
        data: &[u8],
        max_output: Option<usize>,
        _warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
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
        _warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
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
        _warn: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
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
        _: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
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
        _: &mut dyn FnMut(&str, i32) -> PipelineResult<()>,
    ) -> Result<FilterDecodeOutcome> {
        EXPECTED_FIRST_INPUT.with(|expected| {
            assert_eq!(data.as_ptr() as usize, expected.get());
        });
        Ok(FilterDecodeOutcome::complete(data.to_vec()))
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
        _ => None,
    }
}

fn decode_ascii85(data: &[u8], max_output: Option<usize>) -> Result<FilterDecodeOutcome> {
    let mut sink = OutputBuffer::new(max_output);
    let finish_phase = sink.finish_phase();
    let error = {
        let mut stage = Ascii85Decoder::new("ascii85 decode", &mut sink);
        write_and_finish(&mut stage, data, Some(&finish_phase)).map(map_stage_error)
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
    let error = {
        let mut stage = AsciiHexDecoder::new("asciiHex decode", &mut sink);
        write_and_finish(&mut stage, data, Some(&finish_phase)).map(map_stage_error)
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
    let error = {
        let mut stage = RunLength::new("runlength decode", &mut sink, RunLengthAction::Decode);
        write_and_finish(&mut stage, data, Some(&finish_phase)).map(map_stage_error)
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
    decode_flate_chunks([data], max_output, &mut ignore_warning)
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
    decode_params: Option<&Object>,
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
        decode_filter_specs, decode_flate, decode_flate_chunks, encode_flate, encode_run_length,
        ignore_warning, normalize_filter_name, stream_filter_for, FlateLzwStreamFilter,
        OutputBuffer, Pipeline, StreamFilter, DECODE_OUTPUT_LIMIT_PREFIX,
    };
    use crate::{Dictionary, Error, Object};
    use std::cell::RefCell;

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
        let params = Object::Dictionary(Dictionary::new());

        let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs.len(), 2);
        assert!(std::ptr::eq(specs[0].decode_params.unwrap(), &params));
        assert!(std::ptr::eq(specs[1].decode_params.unwrap(), &params));
    }

    #[test]
    fn decode_parms_array_must_align_with_filter_array() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let params = Object::Array(vec![Object::Null]);

        let error = decode_filter_specs(Some(&filter), Some(&params)).unwrap_err();

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

        let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs[0].normalized_name(), b"FlateDecode");
        assert!(specs[0].decode_params.is_none());
    }

    #[test]
    fn no_filter_ignores_decode_parms() {
        let params = Object::Array(vec![Object::Integer(1)]);

        let specs = decode_filter_specs(None, Some(&params)).unwrap();

        assert!(specs.is_empty());
    }

    #[test]
    fn non_name_filter_item_is_rejected_before_decode() {
        let filter = Object::Array(vec![Object::Integer(1)]);

        let error = decode_filter_specs(Some(&filter), None).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn scalar_non_name_filter_is_rejected_before_decode() {
        let error = decode_filter_specs(Some(&Object::Integer(1)), None).unwrap_err();

        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn empty_filter_array_ignores_decode_parms() {
        let filter = Object::Array(Vec::new());
        let params = Object::Array(vec![Object::Integer(1)]);

        assert!(decode_filter_specs(Some(&filter), Some(&params))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn output_buffer_has_qpdf_pipeline_identifier() {
        assert_eq!(OutputBuffer::new(None).identifier(), "stream data buffer");
    }

    #[test]
    fn one_element_decode_parms_array_aligns_with_name_filter() {
        let filter = Object::Name(b"FlateDecode".to_vec());
        let params_item = Object::Dictionary(Dictionary::new());
        let params = Object::Array(vec![params_item.clone()]);

        let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].decode_params, Some(&params_item));
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
            let specs = decode_filter_specs(Some(&filter), None).unwrap();
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
            decode_flate_chunks([b"\x78".as_slice()], None, &mut ignore_warning)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn empty_inflate_skips_codec_and_warning_like_qpdf() {
        let decoded = decode_flate_chunks(std::iter::empty(), None, &mut ignore_warning).unwrap();

        assert!(decoded.is_empty());
    }

    #[test]
    fn malformed_flate_header_retains_qpdf_pipeline_identifier_and_timing() {
        let error = decode_flate_chunks(
            [b"\x78\x00".as_slice(), b"not reached".as_slice()],
            None,
            &mut ignore_warning,
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

        assert!(filter.set_decode_params(None));
        assert!(!filter.is_specialized_compression());
        assert!(!filter.is_lossy_compression());

        let encoded = encode_flate(b"factory pipeline").unwrap();
        let decoded = filter
            .pipe_decode(&encoded, None, &mut |_, _| Ok(()))
            .unwrap();
        assert_eq!(decoded, b"factory pipeline");
    }

    #[test]
    fn flate_factory_treats_non_dictionary_decode_params_as_empty_like_qpdf() {
        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");

        assert!(filter.set_decode_params(Some(&Object::Integer(1))));
    }

    #[test]
    fn flate_lzw_filter_retains_only_the_qpdf_parameter_set() {
        // The adapter keeps the five scalar parameters qpdf keeps, not a
        // reference to the caller's object.
        let params = Object::String(vec![b'x'; 64 * 1024]);
        let mut filter = FlateLzwStreamFilter::new(false);

        assert!(filter.set_decode_params(Some(&params)));
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

    fn accepts(lzw: bool, entries: &[(&str, Object)]) -> bool {
        FlateLzwStreamFilter::new(lzw).set_decode_params(Some(&params(entries)))
    }

    #[test]
    fn flate_lzw_filter_accepts_absent_and_null_decode_params() {
        let mut filter = FlateLzwStreamFilter::new(true);
        assert!(filter.set_decode_params(None));
        assert!(filter.set_decode_params(Some(&Object::Null)));
        assert!(!filter.is_specialized_compression());
        assert!(!filter.is_lossy_compression());
    }

    #[test]
    fn predictor_values_outside_the_supported_set_are_not_filterable() {
        for predictor in [1, 2, 10, 11, 12, 13, 14, 15] {
            assert!(
                accepts(false, &[("Predictor", Object::Integer(predictor))]),
                "predictor {predictor}"
            );
        }
        for predictor in [-1, 0, 3, 9, 16, 100] {
            assert!(
                !accepts(false, &[("Predictor", Object::Integer(predictor))]),
                "predictor {predictor}"
            );
        }
        assert!(!accepts(
            false,
            &[("Predictor", Object::Name(b"12".to_vec()))]
        ));
    }

    #[test]
    fn a_predictor_above_one_requires_a_nonzero_columns_value() {
        assert!(!accepts(
            false,
            &[
                ("Predictor", Object::Integer(12)),
                ("Columns", Object::Integer(0)),
            ]
        ));
        assert!(accepts(
            false,
            &[
                ("Predictor", Object::Integer(1)),
                ("Columns", Object::Integer(0)),
            ]
        ));
        assert!(accepts(
            false,
            &[
                ("Predictor", Object::Integer(12)),
                ("Columns", Object::Integer(4)),
            ]
        ));
    }

    #[test]
    fn geometry_parameters_are_retained_without_range_validation() {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(Some(&params(&[
            ("Predictor", Object::Integer(12)),
            ("Columns", Object::Integer(-4)),
            ("Colors", Object::Integer(-1)),
            ("BitsPerComponent", Object::Integer(99)),
        ]))));
        assert_eq!(
            (filter.columns, filter.colors, filter.bits_per_component),
            (-4, -1, 99)
        );
        assert!(!accepts(false, &[("Columns", Object::Null)]));
        assert!(!accepts(false, &[("Colors", Object::Null)]));
        assert!(!accepts(false, &[("BitsPerComponent", Object::Null)]));
    }

    #[test]
    fn integer_parameters_saturate_at_the_32_bit_boundary() {
        let mut filter = FlateLzwStreamFilter::new(false);
        assert!(filter.set_decode_params(Some(&params(&[
            ("Predictor", Object::Integer(12)),
            ("Columns", Object::Integer(i64::from(i32::MAX) + 10)),
            ("Colors", Object::Integer(i64::from(i32::MIN) - 10)),
        ]))));
        assert_eq!((filter.columns, filter.colors), (i32::MAX, i32::MIN));
    }

    #[test]
    fn early_change_is_read_only_for_lzw_streams() {
        let mut lzw = FlateLzwStreamFilter::new(true);
        assert!(lzw.set_decode_params(Some(&params(&[("EarlyChange", Object::Integer(0))]))));
        assert!(!lzw.early_code_change);

        let mut lzw = FlateLzwStreamFilter::new(true);
        assert!(lzw.set_decode_params(Some(&params(&[("EarlyChange", Object::Integer(1))]))));
        assert!(lzw.early_code_change);

        // A value outside {0, 1} makes an LZW stream unfilterable.
        assert!(!accepts(true, &[("EarlyChange", Object::Integer(7))]));
        assert!(!accepts(
            true,
            &[("EarlyChange", Object::Name(b"1".to_vec()))]
        ));

        // The same parameters are ignored entirely on a Flate stream.
        let mut flate = FlateLzwStreamFilter::new(false);
        assert!(flate.set_decode_params(Some(&params(&[("EarlyChange", Object::Integer(7))]))));
        assert!(flate.early_code_change);
    }

    #[test]
    fn unrecognized_decode_params_keys_are_ignored() {
        assert!(accepts(true, &[("Whatever", Object::Name(b"x".to_vec()))]));
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
        assert!(filter.set_decode_params(Some(&params(&[("EarlyChange", Object::Integer(0))]))));

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
        assert!(filter.set_decode_params(Some(&params(&[
            ("Predictor", Object::Integer(2)),
            ("Columns", Object::Integer(4)),
        ]))));

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
            assert!(filter.set_decode_params(Some(&params(&[
                ("Predictor", Object::Integer(12)),
                ("Columns", Object::Integer(4)),
                (key, Object::Integer(value)),
            ]))));

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
        assert!(filter.set_decode_params(Some(&params(&[
            ("Predictor", Object::Integer(12)),
            ("Columns", Object::Integer(4)),
            ("BitsPerComponent", Object::Integer(3)),
        ]))));

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
        assert!(filter.set_decode_params(Some(&params(&[
            ("Predictor", Object::Integer(12)),
            ("Columns", Object::Integer(4)),
        ]))));

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
        let predicted = params(&[
            ("Predictor", Object::Integer(12)),
            ("Columns", Object::Integer(4)),
        ]);

        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(Some(&predicted)));
        assert_eq!(
            filter
                .pipe_decode(&encoded, Some(8), &mut ignore_warning)
                .expect("eight decoded bytes fit the cap")
                .len(),
            8
        );

        let mut filter = stream_filter_for(b"FlateDecode").expect("registered Flate filter");
        assert!(filter.set_decode_params(Some(&predicted)));
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
    fn ascii_and_run_length_factories_expose_qpdf_stream_filter_contract() {
        for (name, specialized) in [
            (b"ASCII85Decode".as_slice(), false),
            (b"ASCIIHexDecode".as_slice(), false),
            (b"RunLengthDecode".as_slice(), true),
        ] {
            let mut filter = stream_filter_for(name).expect("registered stream filter");

            assert!(filter.set_decode_params(None), "{name:?}");
            assert!(filter.set_decode_params(Some(&Object::Null)), "{name:?}");
            assert!(
                !filter.set_decode_params(Some(&Object::Dictionary(Dictionary::new()))),
                "{name:?}"
            );
            assert!(
                !filter.set_decode_params(Some(&Object::Integer(1))),
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

        assert!(filter.set_decode_params(None));
        assert!(filter.set_decode_params(Some(&Object::Null)));
        assert!(!filter.set_decode_params(Some(&Object::Integer(1))));
        assert_eq!(
            filter
                .pipe_decode(b"test filter", None, &mut ignore_warning)
                .unwrap(),
            b"test filter"
        );
    }
}
