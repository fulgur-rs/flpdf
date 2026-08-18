//! qpdf correspondence: `QPDFJob.cc:1545-1640` (`doJSON` fixed section order) and `QPDFJob.cc:3094-3115` (`writeJSON` output/stream-prefix selection).
//!
//! Responsibility table for this slice:
//!
//! | qpdf responsibility | flpdf owner |
//! | --- | --- |
//! | `doJSONPages`, `doJSONPageLabels`, `doJSONOutlines`, `doJSONAttachments`, `doJSONEncrypt` | `crate::job::build_*_section` |
//! | `doJSONAcroform` | [`crate::json_inspect::build_acroform_section`] (deferred slice) |
//! | `doJSON` fixed order and key selection | this module |
//! | `writeJSON` output destination and side-file prefix | [`write_json`] |

use crate::json::Json;
use crate::json_inspect::{
    DecodeLevel, JsonKey, JsonObjectSelector, JsonOutput, JsonOutputError, StreamDataMode,
    QPDF_JSON_VERSION,
};
use crate::pipeline::stdio_file::StdioBuffer;
use crate::pipeline::{Pipeline, PlOStream, PlStdioFile};
use crate::Pdf;
use std::io::{Read, Seek, Write};
use std::path::Path;

const MISSING_STREAM_PREFIX: &str =
    "please specify --json-stream-prefix since the input file name is unknown";

fn json_section_selected(keys: &[JsonKey], section: JsonKey) -> bool {
    keys.is_empty()
        || keys
            .iter()
            .any(|key| key.output_key_name() == section.output_key_name())
}

fn build_parameters(decode_level: DecodeLevel) -> Result<Json, crate::json_inspect::ConvertError> {
    crate::json_inspect::json_dictionary([(
        "decodelevel",
        Json::make_string(decode_level.as_qpdf_str().as_bytes()),
    )])
}

fn emit_section(
    out: &mut dyn Pipeline,
    first: &mut bool,
    name: &[u8],
    keys: &[JsonKey],
    key: JsonKey,
    build: impl FnOnce() -> Result<Json, crate::json_inspect::ConvertError>,
) -> Result<(), JsonOutputError> {
    if json_section_selected(keys, key) {
        let value = build()?;
        Json::write_dictionary_item(out, first, name, &value, 1)?;
    }
    Ok(())
}

/// Incrementally write a selected qpdf JSON v2 document from the QPDFJob
/// command boundary.
pub fn write_qpdf_json_v2_selected_objects_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError> {
    let mut first = true;
    Json::write_dictionary_open(out, &mut first, 0)?;
    Json::write_dictionary_item(
        out,
        &mut first,
        b"version",
        &Json::make_int(i64::from(QPDF_JSON_VERSION)),
        1,
    )?;
    Json::write_dictionary_item(
        out,
        &mut first,
        b"parameters",
        &build_parameters(decode_level)?,
        1,
    )?;
    emit_section(out, &mut first, b"pages", keys, JsonKey::Pages, || {
        super::json_sections::build_pages_section(pdf)
    })?;
    emit_section(
        out,
        &mut first,
        b"pagelabels",
        keys,
        JsonKey::Pagelabels,
        || super::json_sections::build_pagelabels_section(pdf),
    )?;
    emit_section(
        out,
        &mut first,
        b"acroform",
        keys,
        JsonKey::Acroform,
        || crate::json_inspect::build_acroform_section(pdf),
    )?;
    emit_section(
        out,
        &mut first,
        b"attachments",
        keys,
        JsonKey::Attachments,
        || super::json_sections::build_attachments_section(pdf),
    )?;
    emit_section(out, &mut first, b"encrypt", keys, JsonKey::Encrypt, || {
        super::json_sections::build_encrypt_section(pdf)
    })?;
    emit_section(
        out,
        &mut first,
        b"outlines",
        keys,
        JsonKey::Outlines,
        || super::json_sections::build_outlines_section(pdf),
    )?;
    if json_section_selected(keys, JsonKey::Qpdf) {
        // qpdf's doJSONObjects delegates the whole "qpdf" key to
        // QPDF::writeJSON with complete=false, letting it continue the
        // dictionary this function opened.
        crate::document_json::write_json_key(
            pdf,
            QPDF_JSON_VERSION,
            out,
            false,
            &mut first,
            decode_level,
            stream_mode,
            objects,
        )?;
    }
    Json::write_dictionary_close(out, first, 0)?;
    out.write(b"\n")?;
    Ok(())
}

/// Write selected qpdf JSON v2 output to an ordinary command-boundary handle.
pub fn write_qpdf_json_v2_selected_objects_to_output_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    output: JsonOutput<'_>,
) -> Result<(), JsonOutputError> {
    match output {
        JsonOutput::Stdout(writer) => {
            let mut terminal = PlOStream::new("json output", writer);
            write_qpdf_json_v2_selected_objects_with_options(
                pdf,
                decode_level,
                stream_mode,
                keys,
                objects,
                &mut terminal,
            )?;
            terminal.finish()?;
            Ok(())
        }
        JsonOutput::File(writer) => {
            let mut buffered = StdioBuffer::new(writer);
            {
                let mut terminal = PlStdioFile::new("json output", &mut buffered);
                write_qpdf_json_v2_selected_objects_with_options(
                    pdf,
                    decode_level,
                    stream_mode,
                    keys,
                    objects,
                    &mut terminal,
                )?;
            }
            Ok(())
        }
    }
}

/// Selects how PDF stream payloads appear in JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonStreamData {
    /// Omit stream payloads and emit only stream dictionaries.
    #[default]
    None,
    /// Embed stream payloads as base64 data in the JSON document.
    Inline,
    /// Write stream payloads to side files and refer to their paths from JSON.
    File,
}

/// Unresolved JSON output options accepted at the command boundary.
///
/// [`write_json`] resolves [`JsonStreamData::File`] without an explicit
/// non-empty [`stream_prefix`](Self::stream_prefix) from a file output name
/// when one is available. An empty prefix is treated as absent.
pub struct JsonJobOptions<'a> {
    /// Level of PDF stream decoding before JSON serialization.
    pub decode_level: DecodeLevel,
    /// How stream payloads are represented in the JSON output.
    pub stream_data: JsonStreamData,
    /// Explicit prefix for stream side files, when stream data uses file mode.
    /// An empty prefix is treated as absent.
    pub stream_prefix: Option<&'a str>,
    /// Requested top-level qpdf JSON v2 keys.
    pub keys: &'a [JsonKey],
    /// Requested object selectors for the JSON `objects` section.
    pub objects: &'a [JsonObjectSelector],
}

/// Destination for JSON output at the command boundary.
pub enum JsonJobOutput<'a> {
    /// Standard output, whose writer is finished by the JSON serializer.
    Stdout(&'a mut dyn Write),
    /// A named top-level output file.
    File {
        /// Output filename, used as the default stream side-file prefix.
        filename: &'a Path,
        /// Writer for the top-level JSON file.
        writer: &'a mut dyn Write,
    },
}

/// Invalid combination of command-level JSON output options.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct UsageError {
    message: String,
}

/// Failure while resolving command-level JSON options or writing JSON output.
#[derive(Debug, thiserror::Error)]
pub enum JsonJobError {
    /// An invalid command-level option combination.
    #[error(transparent)]
    Usage(#[from] UsageError),
    /// A failure reported by the delegated JSON serializer or output pipeline.
    #[error(transparent)]
    Output(#[from] JsonOutputError),
    /// A failure while emitting the shared qpdf warning completion state.
    #[error(transparent)]
    Completion(#[from] crate::Error),
}

/// Write qpdf JSON v2 output after resolving command-level stream options.
///
/// This is the `QPDFJob::writeJSON` orchestration boundary: it resolves the
/// stream side-file prefix once, then delegates JSON construction and output
/// lifecycle to the JSON serializer.
///
/// # Errors
///
/// Returns [`JsonJobError::Usage`] when file stream data is requested for
/// standard output without an explicit non-empty prefix. Returns
/// [`JsonJobError::Output`] when the delegated serializer cannot convert PDF
/// data or write its output.
pub fn write_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: JsonJobOptions<'_>,
    output: JsonJobOutput<'_>,
) -> Result<(), JsonJobError> {
    let stream_prefix = options.stream_prefix.filter(|prefix| !prefix.is_empty());
    let stream_mode = match (options.stream_data, stream_prefix, &output) {
        (JsonStreamData::None, _, _) => StreamDataMode::None,
        (JsonStreamData::Inline, _, _) => StreamDataMode::Inline,
        (JsonStreamData::File, Some(prefix), _) => StreamDataMode::File {
            prefix: prefix.to_owned(),
        },
        (JsonStreamData::File, None, JsonJobOutput::File { filename, .. }) => {
            StreamDataMode::File {
                prefix: filename.to_string_lossy().into_owned(),
            }
        }
        (JsonStreamData::File, None, JsonJobOutput::Stdout(_)) => {
            return Err(UsageError {
                message: MISSING_STREAM_PREFIX.to_owned(),
            }
            .into());
        }
    };

    let output = match output {
        JsonJobOutput::Stdout(writer) => JsonOutput::Stdout(writer),
        JsonJobOutput::File { writer, .. } => JsonOutput::File(writer),
    };

    write_qpdf_json_v2_selected_objects_to_output_with_options(
        pdf,
        options.decode_level,
        &stream_mode,
        options.keys,
        options.objects,
        output,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::PlString;
    use std::fs::File;
    use std::io::BufReader;
    use std::path::Path;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf")
    }

    #[test]
    fn job_json_writer_keeps_qpdf_fixed_section_order() {
        let mut pdf = Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap();
        let mut bytes = Vec::new();
        let mut output = PlString::new("job json order", None, &mut bytes);
        let stream_mode = StreamDataMode::None;

        write_qpdf_json_v2_selected_objects_with_options(
            &mut pdf,
            DecodeLevel::Generalized,
            &stream_mode,
            &[],
            &[],
            &mut output,
        )
        .unwrap();

        let positions = [
            "\n  \"version\"",
            "\n  \"parameters\"",
            "\n  \"pages\"",
            "\n  \"pagelabels\"",
            "\n  \"acroform\"",
            "\n  \"attachments\"",
            "\n  \"encrypt\"",
            "\n  \"outlines\"",
            "\n  \"qpdf\"",
        ]
        .map(|key| {
            bytes
                .windows(key.len())
                .position(|window| window == key.as_bytes())
                .unwrap_or_else(|| panic!("missing top-level key {key}"))
        });

        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
