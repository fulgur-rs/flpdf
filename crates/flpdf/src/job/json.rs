//! qpdf correspondence: QPDFJob.cc writeJSON output selection and stream-prefix resolution.

use crate::json_inspect::{
    write_qpdf_json_v2_selected_objects_to_output_with_options, DecodeLevel, JsonKey,
    JsonObjectSelector, JsonOutput, JsonOutputError, StreamDataMode,
};
use crate::Pdf;
use std::io::{Read, Seek, Write};
use std::path::Path;

const MISSING_STREAM_PREFIX: &str =
    "please specify --json-stream-prefix since the input file name is unknown";

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
