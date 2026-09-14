//! qpdf correspondence: `QPDFStreamFilter.cc` and `QPDF_Stream.cc` filter names, full `/DecodeParms` handles, and reverse decode-pipeline construction (`libqpdf/QPDF_Stream.cc:380-482`).
//!
//! Crypt decryption remains owned by
//! `reader::resolver::inspect_stream_encryption` and
//! `reader::resolver::pipe_stream_data_from_input`; this module preserves
//! qpdf's filterability and no-stage pipeline contract.

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

type FilterWarningCallback = Box<dyn FnMut(&str, i32) -> PipelineResult<()> + 'static>;

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
/// codec. The canonical handle reader makes the same factory decision before
/// reading `/DecodeParms`, through `prepare_stream_filter_plan`.
pub(crate) const DECODE_PARMS_LENGTH_ERROR: &str =
    "stream /DecodeParms length is inconsistent with filters";
fn map_pipeline_error(error: PipelineError) -> Error {
    Error::Unsupported(error.into_string_lossy())
}

/// Rust equivalent of qpdf's `QPDFStreamFilter` extension boundary.
///
/// Ordinary stream consumers use `ObjectHandle::pipe_stream_data` directly;
/// this trait only supplies qpdf's filter construction and classification hooks.
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
    #[cfg(test)]
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
            Some(geometry) => make_predictor_pipeline(geometry, next, PredictorAction::Decode)?,
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
}

fn make_predictor_pipeline<'a>(
    geometry: PredictorGeometry,
    next: impl Into<PipelineRef<'a>>,
    action: PredictorAction,
) -> Result<PipelineRef<'a>> {
    let next = next.into();
    let pipeline = match (geometry.kind, action) {
        #[cfg(test)]
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
        #[cfg(test)]
        (PredictorKind::Tiff, PredictorAction::Encode) => Box::new(
            TiffPredictor::new(
                "tiff encode",
                next,
                TiffPredictorAction::Encode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
        (PredictorKind::Tiff, PredictorAction::Decode) => Box::new(
            TiffPredictor::new(
                "tiff decode",
                next,
                TiffPredictorAction::Decode,
                geometry.columns,
                geometry.colors,
                geometry.bits_per_component,
            )
            .map_err(map_pipeline_error)?,
        ) as Box<dyn Pipeline + 'a>,
    };
    Ok(PipelineRef::Owned(pipeline))
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
    /// The key test and its short-circuit order stay inside the loop, as qpdf
    /// evaluates them. In particular, `hasKey` and
    /// `isDictionaryOfType` are reached only for an allowed key, after
    /// `getKeys` has inspected the parameter object; this preserves warning
    /// and resolution timing for non-dictionary and malformed inputs.
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
/// the DCT stage itself is the qpdf-shaped streaming primitive.
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
#[cfg(test)]
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
#[cfg(test)]
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

#[cfg(test)]
fn encode_predictor_stage(data: &[u8], geometry: PredictorGeometry) -> Result<Vec<u8>> {
    let mut sink = Buffer::new("stream data buffer", None);
    {
        let mut predictor = make_predictor_pipeline(geometry, &mut sink, PredictorAction::Encode)?;
        predictor.write(data).map_err(map_pipeline_error)?;
        predictor.finish().map_err(map_pipeline_error)?;
    }
    sink.take_buffer().map_err(map_pipeline_error)
}

#[cfg(test)]
mod tests {
    use super::{FlateLzwStreamFilter, StreamFilter};
    use crate::ObjectHandle;

    #[test]
    fn flate_filter_consumes_qpdf_canonical_slash_prefixed_keys() {
        let mut filter = FlateLzwStreamFilter::new(false);
        let params =
            ObjectHandle::dictionary(vec![(b"/Predictor".to_vec(), ObjectHandle::integer(2))]);

        assert!(filter.set_decode_params(&params).unwrap());
        assert_eq!(filter.predictor, 2);
    }

    #[test]
    fn flate_setter_rejects_non_integer_geometry_parameters() {
        for key in [b"/Predictor".as_slice(), b"/Columns"] {
            let params = ObjectHandle::dictionary(vec![(
                key.to_vec(),
                ObjectHandle::name(b"not-an-integer".to_vec()),
            )]);
            let mut filter = FlateLzwStreamFilter::new(false);
            assert!(!filter.set_decode_params(&params).unwrap());
        }

        let params = ObjectHandle::dictionary(vec![(
            b"/EarlyChange".to_vec(),
            ObjectHandle::name(b"not-an-integer".to_vec()),
        )]);
        let mut filter = FlateLzwStreamFilter::new(true);
        assert!(!filter.set_decode_params(&params).unwrap());
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

        let mut null_filter = super::CryptStreamFilter;
        assert!(null_filter
            .set_decode_params(&ObjectHandle::null())
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
