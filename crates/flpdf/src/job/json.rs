//! qpdf correspondence: `QPDFJob.cc:1545-1640` (`doJSON` fixed section order) and `QPDFJob.cc:3094-3115` (`writeJSON` output/stream-prefix selection).
//!
//! Responsibility table for this slice:
//!
//! | qpdf responsibility | flpdf owner |
//! | --- | --- |
//! | `doJSONPages`, `doJSONPageLabels`, `doJSONOutlines`, `doJSONAttachments`, `doJSONEncrypt` | `crate::job::build_*_section` |
//! | `doJSONAcroform` | [`crate::job::json_sections::build_acroform_section_with_version`] |
//! | `doJSON` fixed order and key selection | this module |
//! | `doJSON` `testJsonSchema` mismatch diagnostics | this module via the job error logger |
//! | `writeJSON` output destination and side-file prefix | [`write_json`] |

use crate::json::Json;
use crate::json_inspect::{
    DecodeLevel, JsonKey, JsonObjectSelector, JsonOutput, JsonOutputError, StreamDataMode,
    QPDF_JSON_VERSION,
};
use crate::pipeline::stdio_file::StdioBuffer;
use crate::pipeline::{Pipeline, PipelineHandle, PlOStream, PlStdioFile};
use crate::{Pdf, QPDFLogger, UsageError};
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

/// Incrementally write a selected qpdf JSON document from the QPDFJob command
/// boundary. Version 1 and version 2 share the section builders but differ in
/// their object-map container (`objects`/`objectinfo` versus `qpdf`).
#[allow(clippy::too_many_arguments)]
pub fn write_qpdf_json_selected_objects_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: i32,
    json_output: bool,
    show_encryption_key: bool,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError> {
    let mut first = true;
    Json::write_dictionary_open(out, &mut first, 0)?;
    if !json_output {
        Json::write_dictionary_item(
            out,
            &mut first,
            b"version",
            &Json::make_int(i64::from(version)),
            1,
        )?;
        Json::write_dictionary_item(
            out,
            &mut first,
            b"parameters",
            &build_parameters(decode_level)?,
            1,
        )?;
    }
    emit_section(out, &mut first, b"pages", keys, JsonKey::Pages, || {
        super::json_sections::build_pages_section_with_options(pdf, version, decode_level)
    })?;
    emit_section(
        out,
        &mut first,
        b"pagelabels",
        keys,
        JsonKey::Pagelabels,
        || super::json_sections::build_pagelabels_section_with_version(pdf, version),
    )?;
    emit_section(
        out,
        &mut first,
        b"acroform",
        keys,
        JsonKey::Acroform,
        || super::json_sections::build_acroform_section_with_version(pdf, version),
    )?;
    emit_section(
        out,
        &mut first,
        b"attachments",
        keys,
        JsonKey::Attachments,
        || super::json_sections::build_attachments_section_with_version(pdf, version),
    )?;
    emit_section(out, &mut first, b"encrypt", keys, JsonKey::Encrypt, || {
        super::json_sections::build_encrypt_section_with_options(pdf, version, show_encryption_key)
    })?;
    emit_section(
        out,
        &mut first,
        b"outlines",
        keys,
        JsonKey::Outlines,
        || super::json_sections::build_outlines_section_with_version(pdf, version),
    )?;
    if version == 1 {
        if json_section_selected(keys, JsonKey::Objects) {
            crate::document_json::write_json_v1_objects_key(pdf, out, &mut first, objects)?;
        } // cov:ignore: llvm-cov continuation
        if json_section_selected(keys, JsonKey::Objectinfo) {
            crate::document_json::write_json_v1_objectinfo_key(pdf, out, &mut first, objects)?;
        } // cov:ignore: llvm-cov continuation
    } else if json_section_selected(keys, JsonKey::Qpdf) {
        // qpdf's doJSONObjects delegates the whole "qpdf" key to
        // QPDF::writeJSON with complete=false, letting it continue the
        // dictionary this function opened.
        crate::document_json::write_json_key(
            pdf,
            version,
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

/// Write selected qpdf JSON output to an ordinary command-boundary handle.
///
/// When schema testing is enabled, a mismatch is reported through
/// `schema_error_output` after the captured JSON has been forwarded, matching
/// `QPDFJob::doJSON` (`libqpdf/QPDFJob.cc:1631-1642`). The mismatch itself does
/// not fail the output operation.
#[allow(clippy::too_many_arguments)]
pub fn write_qpdf_json_selected_objects_to_output_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: i32,
    json_output: bool,
    show_encryption_key: bool,
    test_json_schema: bool,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    keys: &[JsonKey],
    objects: &[JsonObjectSelector],
    output: JsonOutput<'_>,
    schema_error_output: &PipelineHandle,
) -> Result<(), JsonOutputError> {
    match output {
        JsonOutput::Stdout(writer) => {
            let mut terminal = PlOStream::new("json output", writer);
            if test_json_schema {
                let mut captured = Vec::new();
                {
                    let mut capture = crate::pipeline::PlString::new(
                        "capture json",
                        Some(&mut terminal),
                        &mut captured,
                    );
                    write_qpdf_json_selected_objects_with_options(
                        pdf,
                        version,
                        json_output,
                        show_encryption_key,
                        decode_level,
                        stream_mode,
                        keys,
                        objects,
                        &mut capture,
                    )?; // cov:ignore: llvm-cov attributes this successful delegated write to the opening call lines
                }
                validate_json_schema(&captured, version, json_output, keys, schema_error_output)?;
            } else {
                write_qpdf_json_selected_objects_with_options(
                    pdf,
                    version,
                    json_output,
                    show_encryption_key,
                    decode_level,
                    stream_mode,
                    keys,
                    objects,
                    &mut terminal,
                )?;
            }
            terminal.finish()?;
            Ok(())
        }
        JsonOutput::File(writer) => {
            let mut buffered = StdioBuffer::new(writer);
            {
                let mut terminal = PlStdioFile::new("json output", &mut buffered);
                if test_json_schema {
                    let mut captured = Vec::new();
                    {
                        let mut capture = crate::pipeline::PlString::new(
                            "capture json",
                            Some(&mut terminal),
                            &mut captured,
                        );
                        write_qpdf_json_selected_objects_with_options(
                            pdf,
                            version,
                            json_output,
                            show_encryption_key,
                            decode_level,
                            stream_mode,
                            keys,
                            objects,
                            &mut capture,
                        )?; // cov:ignore: llvm-cov attributes this successful delegated write to the opening call lines
                    }
                    validate_json_schema(
                        &captured,
                        version,
                        json_output,
                        keys,
                        schema_error_output,
                    )?; // cov:ignore: llvm-cov attributes this successful schema validation to the opening call lines
                } else {
                    write_qpdf_json_selected_objects_with_options(
                        pdf,
                        version,
                        json_output,
                        show_encryption_key,
                        decode_level,
                        stream_mode,
                        keys,
                        objects,
                        &mut terminal,
                    )?;
                }
            }
            Ok(())
        }
    }
}

fn schema_dictionary(
    entries: impl IntoIterator<Item = (&'static str, Json)>,
) -> Result<Json, JsonOutputError> {
    crate::json_inspect::json_dictionary(entries).map_err(JsonOutputError::from)
}

fn schema_array(item: Json) -> Result<Json, JsonOutputError> {
    let array = Json::make_array();
    array.add_array_element(item).map_err(|error| {
        // cov:ignore-start: a freshly constructed array cannot reject an array element
        JsonOutputError::Convert(crate::json_inspect::ConvertError::JsonError(
            error.to_string(),
        ))
        // cov:ignore-end
    })?; // cov:ignore: a freshly constructed array cannot reject an array element
    Ok(array)
}

fn schema_fixed_array(items: impl IntoIterator<Item = Json>) -> Result<Json, JsonOutputError> {
    let array = Json::make_array();
    for item in items {
        array.add_array_element(item).map_err(|error| {
            // cov:ignore-start: a freshly constructed array cannot reject an array element
            JsonOutputError::Convert(crate::json_inspect::ConvertError::JsonError(
                error.to_string(),
            ))
            // cov:ignore-end
        })?; // cov:ignore: a freshly constructed array cannot reject an array element
    }
    Ok(array)
}

fn schema_pattern(item: Json) -> Result<Json, JsonOutputError> {
    schema_dictionary([("<key>", item)])
}

fn selected(keys: &[JsonKey], key: JsonKey) -> bool {
    keys.is_empty() || keys.contains(&key)
}

fn output_schema(
    version: i32,
    json_output: bool,
    keys: &[JsonKey],
) -> Result<Json, JsonOutputError> {
    let scalar = || Json::make_string("qpdf JSON schema value");
    let mut entries = Vec::new();

    if !json_output {
        entries.push(("version", scalar()));
        entries.push((
            "parameters",
            schema_dictionary([("decodelevel", scalar())])?,
        ));
    }

    if selected(keys, JsonKey::Pages) {
        let image = schema_dictionary([
            ("bitspercomponent", scalar()),
            ("colorspace", scalar()),
            ("decodeparms", schema_array(scalar())?),
            ("filter", schema_array(scalar())?),
            ("filterable", scalar()),
            ("height", scalar()),
            ("name", scalar()),
            ("object", scalar()),
            ("width", scalar()),
        ])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        let page_outline = schema_dictionary([
            ("dest", scalar()),
            ("object", scalar()),
            ("title", scalar()),
        ])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        let page = schema_dictionary([
            ("contents", schema_array(scalar())?),
            ("images", schema_array(image)?),
            ("label", scalar()),
            ("object", scalar()),
            ("outlines", schema_array(page_outline)?),
            ("pageposfrom1", scalar()),
        ])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        entries.push(("pages", schema_array(page)?));
    }

    if selected(keys, JsonKey::Pagelabels) {
        entries.push((
            "pagelabels",
            schema_array(schema_dictionary([
                ("index", scalar()),
                ("label", scalar()),
            ])?)?,
        ));
    }

    if selected(keys, JsonKey::Outlines) {
        let outline = schema_dictionary([
            ("dest", scalar()),
            ("destpageposfrom1", scalar()),
            ("kids", scalar()),
            ("object", scalar()),
            ("open", scalar()),
            ("title", scalar()),
        ])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        entries.push(("outlines", schema_array(outline)?));
    }

    if selected(keys, JsonKey::Acroform) {
        let annotation = schema_dictionary([
            ("annotationflags", scalar()),
            ("appearancestate", scalar()),
            ("object", scalar()),
        ])?;
        let field = schema_dictionary([
            ("alternativename", scalar()),
            ("annotation", annotation),
            ("choices", scalar()),
            ("defaultvalue", scalar()),
            ("fieldflags", scalar()),
            ("fieldtype", scalar()),
            ("fullname", scalar()),
            ("ischeckbox", scalar()),
            ("ischoice", scalar()),
            ("isradiobutton", scalar()),
            ("istext", scalar()),
            ("mappingname", scalar()),
            ("object", scalar()),
            ("pageposfrom1", scalar()),
            ("parent", scalar()),
            ("partialname", scalar()),
            ("quadding", scalar()),
            ("value", scalar()),
        ])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        entries.push((
            "acroform",
            schema_dictionary([
                ("fields", schema_array(field)?),
                ("hasacroform", scalar()),
                ("needappearances", scalar()),
            ])?, // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        )); // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
    }

    if selected(keys, JsonKey::Encrypt) {
        let modify_annotations = if version == 1 {
            "moddifyannotations"
        } else {
            "modifyannotations"
        };
        let capabilities = schema_dictionary([
            ("accessibility", scalar()),
            ("extract", scalar()),
            (modify_annotations, scalar()),
            ("modify", scalar()),
            ("modifyassembly", scalar()),
            ("modifyforms", scalar()),
            ("modifyother", scalar()),
            ("printhigh", scalar()),
            ("printlow", scalar()),
        ])?;
        let parameters = schema_dictionary([
            ("P", scalar()),
            ("R", scalar()),
            ("V", scalar()),
            ("bits", scalar()),
            ("filemethod", scalar()),
            ("key", scalar()),
            ("method", scalar()),
            ("streammethod", scalar()),
            ("stringmethod", scalar()),
        ])?;
        entries.push((
            "encrypt",
            schema_dictionary([
                ("capabilities", capabilities),
                ("encrypted", scalar()),
                ("ownerpasswordmatched", scalar()),
                ("parameters", parameters),
                ("recovereduserpassword", scalar()),
                ("userpasswordmatched", scalar()),
            ])?,
        )); // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
    }

    if selected(keys, JsonKey::Attachments) {
        let stream = schema_dictionary([
            ("checksum", scalar()),
            ("creationdate", scalar()),
            ("mimetype", scalar()),
            ("modificationdate", scalar()),
        ])?;
        let attachment = schema_dictionary([
            ("description", scalar()),
            ("filespec", scalar()),
            ("names", schema_pattern(scalar())?),
            ("preferredcontents", scalar()),
            ("preferredname", scalar()),
            ("streams", schema_pattern(stream)?),
        ])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        entries.push(("attachments", schema_pattern(attachment)?));
    }

    if version == 1 {
        if selected(keys, JsonKey::Objects) {
            entries.push(("objects", schema_pattern(scalar())?)); // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        } // cov:ignore: llvm-cov attributes this successful schema branch continuation to its entry expressions
        if selected(keys, JsonKey::Objectinfo) {
            let stream =
                schema_dictionary([("filter", scalar()), ("is", scalar()), ("length", scalar())])?; // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
            entries.push((
                "objectinfo",
                schema_pattern(schema_dictionary([("stream", stream)])?)?,
            )); // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
        } // cov:ignore: llvm-cov attributes this successful schema branch continuation to its entry expressions
    } else if selected(keys, JsonKey::Qpdf) {
        let metadata = schema_dictionary([
            ("calledgetallpages", scalar()),
            ("jsonversion", scalar()),
            ("maxobjectid", scalar()),
            ("pdfversion", scalar()),
            ("pushedinheritedpageresources", scalar()),
        ])?;
        entries.push((
            "qpdf",
            schema_fixed_array([metadata, schema_pattern(scalar())?])?,
        )); // cov:ignore: llvm-cov attributes this successful schema construction to its entry expressions
    } // cov:ignore: llvm-cov attributes this successful schema branch continuation to its entry expressions

    schema_dictionary(entries)
}

fn validate_json_schema(
    bytes: &[u8],
    version: i32,
    json_output: bool,
    keys: &[JsonKey],
    error_output: &PipelineHandle,
) -> Result<(), JsonOutputError> {
    let value = Json::parse(bytes).map_err(|error| {
        JsonOutputError::Convert(crate::json_inspect::ConvertError::JsonError(
            error.to_string(),
        ))
    })?;
    let schema = output_schema(version, json_output, keys)?;
    let mut errors = Vec::new();
    if !value.check_schema(&schema, &mut errors) {
        error_output.write(b"QPDFJob didn't create JSON that complies with its own rules.\n")?;
        for error in errors {
            error_output.write(error.as_bytes())?;
            error_output.write(b"\n")?;
        }
    }
    Ok(())
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
    pub stream_prefix: Option<&'a [u8]>,
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

impl From<JsonJobError> for crate::Error {
    fn from(error: JsonJobError) -> Self {
        match error {
            JsonJobError::Usage(error) => Self::Usage(error),
            JsonJobError::Output(error) => error.into(),
            JsonJobError::Completion(error) => error,
        }
    }
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
    write_json_with_version(pdf, QPDF_JSON_VERSION, false, false, false, options, output)
}

/// Write qpdf JSON output for an explicitly selected version and optionally
/// validate the generated bytes against the matching qpdf output schema.
///
/// A schema mismatch is written to the default qpdf error logger and does not
/// fail the operation. `QPDFJob` uses its own logger through the internal
/// `write_json_with_version_with_logger` boundary.
pub fn write_json_with_version<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: i32,
    test_json_schema: bool,
    json_output: bool,
    show_encryption_key: bool,
    options: JsonJobOptions<'_>,
    output: JsonJobOutput<'_>,
) -> Result<(), JsonJobError> {
    let logger = QPDFLogger::default_logger();
    write_json_with_version_with_logger(
        pdf,
        version,
        test_json_schema,
        json_output,
        show_encryption_key,
        options,
        output,
        &logger,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_json_with_version_with_logger<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: i32,
    test_json_schema: bool,
    json_output: bool,
    show_encryption_key: bool,
    options: JsonJobOptions<'_>,
    output: JsonJobOutput<'_>,
    logger: &QPDFLogger,
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
                prefix: path_bytes(filename),
            }
        }
        (JsonStreamData::File, None, JsonJobOutput::Stdout(_)) => {
            return Err(UsageError::new(MISSING_STREAM_PREFIX).into());
        }
    };

    let output = match output {
        JsonJobOutput::Stdout(writer) => JsonOutput::Stdout(writer),
        JsonJobOutput::File { writer, .. } => JsonOutput::File(writer),
    };
    let schema_error_output = logger.get_error()?;

    write_qpdf_json_selected_objects_to_output_with_options(
        pdf,
        version,
        json_output,
        show_encryption_key,
        test_json_schema,
        options.decode_level,
        &stream_mode,
        options.keys,
        options.objects,
        output,
        &schema_error_output,
    )?;
    Ok(())
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::PlString;
    use crate::pipeline::{PipelineHandle, PipelineResult};
    use std::fs::File;
    use std::io::BufReader;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    struct RecordingErrorSink(Arc<Mutex<Vec<u8>>>);

    impl Pipeline for RecordingErrorSink {
        fn identifier(&self) -> &str {
            "json schema error"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf")
    }

    #[test]
    fn job_json_writer_keeps_qpdf_fixed_section_order() {
        let mut pdf = Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap();
        let mut bytes = Vec::new();
        let mut output = PlString::new("job json order", None, &mut bytes);
        let stream_mode = StreamDataMode::None;

        write_qpdf_json_selected_objects_with_options(
            &mut pdf,
            QPDF_JSON_VERSION,
            false,
            false,
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

    #[test]
    fn json_job_error_conversion_preserves_usage_output_and_completion() {
        let usage: crate::Error = JsonJobError::Usage(UsageError::new("usage")).into();
        assert!(matches!(usage, crate::Error::Usage(error) if error.to_string() == "usage"));

        let output: crate::Error = JsonJobError::Output(JsonOutputError::UnsupportedVersion).into();
        assert!(matches!(
            output,
            crate::Error::System(message)
                if message == "QPDF::writeJSON: only version 2 is supported"
        ));

        let completion: crate::Error =
            JsonJobError::Completion(crate::Error::System("completion".to_owned())).into();
        assert!(matches!(
            completion,
            crate::Error::System(message) if message == "completion"
        ));
    }

    #[test]
    fn job_json_writer_emits_the_v1_object_and_objectinfo_sections() {
        let mut pdf = Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap();
        let mut bytes = Vec::new();
        let keys = [JsonKey::Objects, JsonKey::Objectinfo];
        let mut output = PlString::new("job json v1", None, &mut bytes);

        write_qpdf_json_selected_objects_with_options(
            &mut pdf,
            1,
            false,
            false,
            DecodeLevel::None,
            &StreamDataMode::None,
            &keys,
            &[],
            &mut output,
        )
        .unwrap();

        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"objects\""));
        assert!(text.contains("\"objectinfo\""));
        assert!(text.contains("\"stream\""));
    }

    #[test]
    fn job_json_v1_selectors_skip_unselected_objects_but_keep_the_trailer() {
        let mut pdf = Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap();
        let mut bytes = Vec::new();
        let keys = [JsonKey::Objects, JsonKey::Objectinfo];
        let selectors = [
            JsonObjectSelector::Object {
                number: 1,
                generation: 0,
            },
            JsonObjectSelector::Trailer,
        ];
        let mut output = PlString::new("job json v1 selected", None, &mut bytes);

        write_qpdf_json_selected_objects_with_options(
            &mut pdf,
            1,
            false,
            false,
            DecodeLevel::None,
            &StreamDataMode::None,
            &keys,
            &selectors,
            &mut output,
        )
        .unwrap();

        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"1 0 R\""));
        assert!(text.contains("\"trailer\""));
    }

    #[test]
    fn job_json_schema_validation_covers_stdout_and_file_boundaries() {
        let keys = [JsonKey::Qpdf];
        let stream_mode = StreamDataMode::None;
        let error_output = QPDFLogger::default_logger().get_error().unwrap();
        let mut stdout = Vec::new();
        let mut pdf = Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap();
        write_qpdf_json_selected_objects_to_output_with_options(
            &mut pdf,
            2,
            false,
            false,
            true,
            DecodeLevel::None,
            &stream_mode,
            &keys,
            &[],
            JsonOutput::Stdout(&mut stdout),
            &error_output,
        )
        .unwrap();
        assert!(!stdout.is_empty());

        let mut file_output = Vec::new();
        let mut pdf = Pdf::open(BufReader::new(File::open(fixture()).unwrap())).unwrap();
        write_qpdf_json_selected_objects_to_output_with_options(
            &mut pdf,
            2,
            true,
            false,
            true,
            DecodeLevel::None,
            &stream_mode,
            &keys,
            &[],
            JsonOutput::File(&mut file_output),
            &error_output,
        )
        .unwrap();
        assert!(!file_output.is_empty());
    }

    #[test]
    fn job_json_schema_reports_parse_and_shape_failures() {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        let mut sink = RecordingErrorSink(Arc::clone(&errors));
        assert_eq!(sink.identifier(), "json schema error");
        sink.finish().unwrap();
        logger.set_error(Some(PipelineHandle::new(sink)));
        let error_output = logger.get_error().unwrap();

        assert!(matches!(
            validate_json_schema(b"{", 2, false, &[], &error_output),
            Err(JsonOutputError::Convert(_))
        ));
        assert!(validate_json_schema(b"{}", 2, false, &[JsonKey::Qpdf], &error_output).is_ok());
        assert_eq!(
            String::from_utf8(errors.lock().unwrap().clone()).unwrap(),
            "QPDFJob didn't create JSON that complies with its own rules.\n\
top-level object: key \"parameters\" is present in schema but missing in object\n\
top-level object: key \"qpdf\" is present in schema but missing in object\n\
top-level object: key \"version\" is present in schema but missing in object\n"
        );

        output_schema(
            1,
            false,
            &[
                JsonKey::Pages,
                JsonKey::Pagelabels,
                JsonKey::Outlines,
                JsonKey::Acroform,
                JsonKey::Encrypt,
                JsonKey::Attachments,
                JsonKey::Objects,
                JsonKey::Objectinfo,
            ],
        )
        .unwrap();
        output_schema(2, true, &[]).unwrap();
    }
}
