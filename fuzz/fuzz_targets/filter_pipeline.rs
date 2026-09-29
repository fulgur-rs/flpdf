#![no_main]

//! Fuzz qpdf's public stream-data pipeline with arbitrary filter arrays,
//! payload bytes, and aligned `/DecodeParms` values.
//!
//! Each input constructs a document-owned stream and calls
//! `ObjectHandle::pipe_stream_data`, which reaches flpdf's canonical decode
//! chain without exposing the private pipeline builder. Parse/decode errors
//! are expected; panic, abort, sanitizer failure, OOM, or timeout is a bug.

use std::io::{Read, Seek};
use std::rc::Rc;
use std::sync::Arc;

use flpdf::writer::DecodeLevel;
use flpdf::{ObjectHandle, Pdf, PdfOpenOptions, Pipeline, PipelineError, PipelineResult};
use libfuzzer_sys::fuzz_target;

const BASE_PDF: &[u8] = include_bytes!("../seeds/roundtrip/minimal.pdf");
const MAX_FILTER_CHAIN: usize = 17;
const MAX_RUNLENGTH_STAGES: usize = 1;
const MAX_DECODED_OUTPUT_BYTES: usize = 1 << 20;
const MAX_DECODED_WRITE_CALLS: usize = 4_096;
const FILTER_NAMES: [&[u8]; 9] = [
    b"FlateDecode",
    b"ASCIIHexDecode",
    b"ASCII85Decode",
    b"RunLengthDecode",
    b"LZWDecode",
    b"CCITTFaxDecode",
    b"DCTDecode",
    b"JPXDecode",
    b"JBIG2Decode",
];
const PNG_PREDICTORS: [i64; 8] = [1, 2, 10, 11, 12, 13, 14, 15];
const SAMPLE_BITS: [i64; 5] = [1, 2, 4, 8, 16];

/// Stop a fuzz iteration before a filter chain amplifies a small input into
/// excessive output or downstream calls. These limits belong to the harness;
/// product pipelines keep qpdf's stream behavior.
struct DecodedOutputBudget {
    written: usize,
    write_calls: usize,
}

impl Pipeline for DecodedOutputBudget {
    fn identifier(&self) -> &str {
        "filter fuzz output budget"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.write_calls += 1;
        if self.write_calls > MAX_DECODED_WRITE_CALLS {
            return Err(PipelineError::runtime(
                "fuzz decoded write-call budget exceeded",
            ));
        }
        let written = self
            .written
            .checked_add(data.len())
            .ok_or_else(|| PipelineError::runtime("fuzz decoded output length overflow"))?;
        if written > MAX_DECODED_OUTPUT_BYTES {
            return Err(PipelineError::runtime(
                "fuzz decoded output budget exceeded",
            ));
        }
        self.written = written;
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn integer(value: u32) -> ObjectHandle {
    ObjectHandle::integer(i64::from(value))
}

fn decode_parameters(filter: &[u8], selector: u8) -> ObjectHandle {
    match filter {
        b"FlateDecode" | b"LZWDecode" => {
            let is_flate = filter == b"FlateDecode";
            let (predictor, columns, colors, bits_per_component) = if is_flate && selector == 0xff {
                // qpdf's u32 row-width expression wraps this geometry to zero,
                // which must fail before a row buffer is allocated.
                (12, 1_u32 << 29, 1, 8)
            } else if !is_flate && selector == 0xfe {
                // This qpdf-int-representable width wraps the TIFF row
                // geometry to zero and is rejected before its row buffer is
                // resized.
                (2, 1_u32 << 29, 1, 8)
            } else {
                let predictor = PNG_PREDICTORS[usize::from(selector) % PNG_PREDICTORS.len()];
                let columns = 1 + u32::from(selector.rotate_left(2) % 32);
                let colors = 1 + u32::from(selector.rotate_left(4) % 4);
                let bits = SAMPLE_BITS[usize::from(selector.rotate_right(1)) % SAMPLE_BITS.len()];
                (predictor, columns, colors, bits)
            };
            let mut entries = vec![
                (b"Predictor".to_vec(), ObjectHandle::integer(predictor)),
                (b"Columns".to_vec(), integer(columns)),
                (b"Colors".to_vec(), integer(colors)),
                (
                    b"BitsPerComponent".to_vec(),
                    ObjectHandle::integer(bits_per_component),
                ),
            ];
            if !is_flate {
                entries.push((
                    b"EarlyChange".to_vec(),
                    ObjectHandle::integer(i64::from(selector & 1)),
                ));
            }
            ObjectHandle::dictionary(entries)
        }
        b"CCITTFaxDecode" => ObjectHandle::dictionary(vec![
            (
                b"K".to_vec(),
                ObjectHandle::integer(i64::from(selector % 3) - 1),
            ),
            (
                b"Columns".to_vec(),
                ObjectHandle::integer(1 + i64::from(selector % 32)),
            ),
            (
                b"Rows".to_vec(),
                ObjectHandle::integer(i64::from(selector.rotate_left(3) % 32)),
            ),
            (
                b"BlackIs1".to_vec(),
                ObjectHandle::boolean(selector & 1 != 0),
            ),
            (
                b"EndOfLine".to_vec(),
                ObjectHandle::boolean(selector & 2 != 0),
            ),
            (
                b"EncodedByteAlign".to_vec(),
                ObjectHandle::boolean(selector & 4 != 0),
            ),
        ]),
        b"DCTDecode" => ObjectHandle::dictionary(vec![(
            b"ColorTransform".to_vec(),
            ObjectHandle::integer(i64::from(selector & 1)),
        )]),
        _ => ObjectHandle::null(),
    }
}

fn pipe_filter_chain<R: Read + Seek + 'static>(pdf: &Pdf<R>, data: &[u8]) {
    let first = data.first().copied().unwrap_or_default();
    let requested_count = 1 + usize::from(first) % MAX_FILTER_CHAIN;
    let parameter_start = 1 + requested_count;
    let payload_start = parameter_start + requested_count;
    let mut runlength_stages = 0;
    let filters = (0..requested_count)
        .filter_map(|stage| {
            let selector = data.get(stage + 1).copied().unwrap_or(stage as u8);
            let filter = FILTER_NAMES[usize::from(selector) % FILTER_NAMES.len()];
            if filter == b"RunLengthDecode" {
                runlength_stages += 1;
                if runlength_stages > MAX_RUNLENGTH_STAGES {
                    return None;
                }
            }
            Some((stage, filter))
        })
        .collect::<Vec<_>>();
    let decode_parameters = filters
        .iter()
        .map(|(stage, filter)| {
            decode_parameters(
                filter,
                data.get(parameter_start + *stage)
                    .copied()
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    let payload = data.get(payload_start..).unwrap_or_default();

    let filter_values = filters
        .iter()
        .map(|(_, name)| ObjectHandle::name(name.to_vec()))
        .collect::<Vec<_>>();
    let filter_value = if filter_values.len() == 1 {
        filter_values[0].clone()
    } else {
        ObjectHandle::array(filter_values)
    };

    let stream = match pdf.new_stream_with_data(Rc::new(payload.to_vec())) {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let Some(dictionary) = stream.as_stream_dict() else {
        return;
    };
    if dictionary.replace_key(b"/Filter", filter_value).is_err()
        || dictionary
            .replace_key(b"/DecodeParms", ObjectHandle::array(decode_parameters))
            .is_err()
    {
        return;
    }

    let mut sink = DecodedOutputBudget {
        written: 0,
        write_calls: 0,
    };
    let mut filtering_attempted = false;
    let _ = stream.pipe_stream_data(
        &mut sink,
        &mut filtering_attempted,
        0,
        DecodeLevel::All,
        true,
        false,
    );
}

fuzz_target!(|data: &[u8]| {
    let base: Arc<[u8]> = Arc::from(BASE_PDF);
    let pdf = flpdf::Pdf::open_mem_with_options(
        Arc::clone(&base),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    );
    if let Ok(pdf) = pdf {
        pipe_filter_chain(&pdf, data);
    }
});
