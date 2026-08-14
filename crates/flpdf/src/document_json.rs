//! qpdf correspondence: QPDF_json.cc output side — the free function `writeJSONStreamFile` and both `QPDF::writeJSON` overloads; the input side (`JSONReactor`, `createFromJSON`, `updateFromJSON`, `importJSON`) has no counterpart here.
//! Write a document's `qpdf` JSON v2 key.
//!
//! The `qpdf` key is a two-element array: a fixed metadata object followed by
//! the raw object map, in which every selected indirect object appears under an
//! `obj:N M R` key and the trailer under `trailer`. qpdf 11.9.0 keeps this
//! serialization separate from the section builders that assemble the
//! surrounding JSON document, and this module holds the same boundary; the
//! builders live in [`crate::json_inspect`].
//!
//! Only JSON version 2 is supported. Any other version is rejected with
//! [`JsonOutputError::UnsupportedVersion`].
//!
//! Per-object value serialization is not part of this boundary — qpdf
//! delegates it to `QPDFObjectHandle::writeJSON`, whose flpdf counterpart is
//! still in [`crate::json_inspect`]. Per-stream serialization
//! (`QPDF_Stream::writeStreamJSON`) is not separated either: it stays inlined
//! in the object-entry writers below, because the entry writers derive the
//! emitted stream dictionary before writing the object key and moving that
//! derivation behind a call boundary would change which bytes reach the sink
//! when the conversion fails.

use crate::json::Json;
use crate::json_inspect::{
    normalized_emitted_stream_dict, ordered_qpdf_dict, qpdf_resolve_top_level_object,
    side_file_io_error, stream_payload_with_decode_status, ConvertError, DecodeLevel,
    JsonObjectSelector, JsonOutputError, StreamDataMode,
};
use crate::object::{Object, ObjectRef, Stream};
use crate::object_handle::ObjectHandle;
use crate::pipeline::stdio_file::StdioBuffer;
use crate::pipeline::{Pipeline, PlStdioFile};
use crate::Pdf;
use std::fs::File;
use std::io::{Read, Seek, Write};

/// The only qpdf JSON version whose serialization is defined here.
const SUPPORTED_JSON_VERSION: i32 = 2;

/// Format the side-file path for a `File`-mode stream entry.
///
/// qpdf 11.9.0 names side files `<prefix>-<obj_num>` — the bare object
/// number with no zero-padding. Centralized here so the JSON `datafile`
/// value and the side-file writer always produce the same name.
pub fn format_json_side_file_path(prefix: &str, obj_num: u32) -> String {
    format!("{prefix}-{obj_num}")
}

/// Write a complete JSON document containing only the `qpdf` key.
///
/// The document is opened, the `qpdf` key written at depth 1, and the sink
/// finished, matching qpdf's public single-key writer. Use
/// [`write_json_key`] to add the same key to a dictionary that is already
/// open.
///
/// `wanted_objects` selects entries of the raw object map; an empty slice
/// selects every object and the trailer. Selection never changes the metadata
/// object, whose `maxobjectid` still reflects the whole document.
///
/// # Errors
///
/// Returns [`JsonOutputError::UnsupportedVersion`] unless `version` is 2,
/// [`JsonOutputError::Convert`] when a PDF object cannot be represented as
/// JSON, [`JsonOutputError::SideFileIo`] when a stream side file cannot be
/// created or written, and [`JsonOutputError::Pipeline`] when the sink rejects
/// a write or its finish.
pub fn write_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: i32,
    out: &mut dyn Pipeline,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    wanted_objects: &[JsonObjectSelector],
) -> Result<(), JsonOutputError> {
    let mut first_key = true;
    write_json_key(
        pdf,
        version,
        out,
        true,
        &mut first_key,
        decode_level,
        stream_mode,
        wanted_objects,
    )
}

/// Write the `qpdf` key, optionally as a complete JSON document.
///
/// When `complete` is true a whole JSON object containing only the `qpdf` key
/// is written and the sink is finished. When it is false the key and its value
/// are written into a dictionary the caller has already opened; `first_key`
/// tells the writer whether a separating comma is needed and is set to false
/// on return.
///
/// # Errors
///
/// Same as [`write_json`].
#[allow(clippy::too_many_arguments)]
pub fn write_json_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: i32,
    out: &mut dyn Pipeline,
    complete: bool,
    first_key: &mut bool,
    decode_level: DecodeLevel,
    stream_mode: &StreamDataMode,
    wanted_objects: &[JsonObjectSelector],
) -> Result<(), JsonOutputError> {
    if version != SUPPORTED_JSON_VERSION {
        return Err(JsonOutputError::UnsupportedVersion);
    }
    if complete {
        Json::write_dictionary_open(out, first_key, 0)?;
    }
    Json::write_dictionary_key(out, first_key, b"qpdf", 1)?;
    let mut qpdf_first = true;
    Json::write_array_open(out, &mut qpdf_first, 1)?;
    Json::write_next(out, &mut qpdf_first, 2)?;
    let mut metadata_first = true;
    Json::write_dictionary_open(out, &mut metadata_first, 2)?;
    Json::write_dictionary_item(
        out,
        &mut metadata_first,
        b"jsonversion",
        &Json::make_int(i64::from(version)),
        3,
    )?;
    Json::write_dictionary_item(
        out,
        &mut metadata_first,
        b"pdfversion",
        &Json::make_string(pdf.version()),
        3,
    )?;
    Json::write_dictionary_item(
        out,
        &mut metadata_first,
        b"pushedinheritedpageresources",
        &Json::make_bool(false),
        3,
    )?;
    Json::write_dictionary_item(
        out,
        &mut metadata_first,
        b"calledgetallpages",
        &Json::make_bool(pdf.ever_called_get_all_pages()),
        3,
    )?;
    Json::write_dictionary_key(out, &mut metadata_first, b"maxobjectid", 3)?;
    let max_object_id = pdf.get_object_count().map_err(ConvertError::from)?;
    Json::make_int(i64::from(max_object_id)).write(out, 3)?;
    Json::write_dictionary_close(out, metadata_first, 2)?;

    Json::write_next(out, &mut qpdf_first, 2)?;
    let mut objects_first = true;
    Json::write_dictionary_open(out, &mut objects_first, 2)?;
    let objects = pdf.get_all_objects().map_err(ConvertError::from)?;
    for handle in objects {
        let object_ref = handle
            .object_ref()
            .expect("qpdf object-map entries are indirect handles");
        if !object_selected(wanted_objects, object_ref) {
            continue;
        }
        let result = match stream_mode {
            StreamDataMode::None => write_non_file_mode_object_entry(
                pdf,
                &handle,
                decode_level,
                NonFileStreamDataMode::None,
                out,
                &mut objects_first,
            ),
            StreamDataMode::Inline => write_non_file_mode_object_entry(
                pdf,
                &handle,
                decode_level,
                NonFileStreamDataMode::Inline,
                out,
                &mut objects_first,
            ),
            StreamDataMode::File { prefix } => write_file_mode_object_entry(
                pdf,
                &handle,
                decode_level,
                prefix,
                out,
                &mut objects_first,
            ),
        };
        result?;
    }

    if trailer_selected(wanted_objects) {
        let trailer = pdf.trailer_handle();
        Json::write_dictionary_key(out, &mut objects_first, b"trailer", 3)?;
        let mut trailer_first = true;
        Json::write_dictionary_open(out, &mut trailer_first, 3)?;
        Json::write_dictionary_key(out, &mut trailer_first, b"value", 4)?;
        trailer.write_json(SUPPORTED_JSON_VERSION, out, true, 4)?;
        Json::write_dictionary_close(out, trailer_first, 3)?;
    }
    // qpdf keeps the raw object map expanded even when selectors match
    // neither an object nor the trailer: `{\n    }`, not compact `{}`.
    Json::write_dictionary_close(out, false, 2)?;
    Json::write_array_close(out, qpdf_first, 1)?;
    if complete {
        Json::write_dictionary_close(out, false, 0)?;
        out.write(b"\n")?;
        out.finish()?;
    }
    Ok(())
}

fn object_selected(selectors: &[JsonObjectSelector], object_ref: ObjectRef) -> bool {
    selectors.is_empty()
        || selectors.iter().any(|selector| {
            matches!(
                selector,
                JsonObjectSelector::Object { number, generation }
                    if *number == object_ref.number
                        && *generation == object_ref.generation
            )
        })
}

fn trailer_selected(selectors: &[JsonObjectSelector]) -> bool {
    selectors.is_empty()
        || selectors
            .iter()
            .any(|selector| matches!(selector, JsonObjectSelector::Trailer))
}

pub(crate) enum NonFileStreamDataMode {
    None,
    Inline,
}

fn write_non_file_mode_object_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    decode_level: DecodeLevel,
    stream_mode: NonFileStreamDataMode,
    out: &mut dyn Pipeline,
    objects_first: &mut bool,
) -> Result<(), JsonOutputError> {
    let object_ref = handle
        .object_ref()
        .expect("qpdf object-map entries are indirect handles");
    let key = format!("obj:{} {} R", object_ref.number, object_ref.generation);

    handle.try_dereference().map_err(ConvertError::from)?;
    if handle.type_code() == 10 {
        // Stream payload/retry behavior remains the QPDF_Stream::writeStreamJSON
        // boundary owned by flpdf-3yn9.9. Keep that narrow legacy adapter until
        // the stream consumer moves, while non-stream values use the canonical
        // ObjectHandle writer below.
        let Object::Stream(stream) = qpdf_resolve_top_level_object(pdf, object_ref)? else {
            return Err(ConvertError::PdfError(
                "canonical stream handle has no legacy stream payload".to_string(),
            )
            .into());
        };
        {
            let (data, dict) = match stream_mode {
                NonFileStreamDataMode::None => (None, ordered_qpdf_dict(pdf, &stream.dict)?),
                NonFileStreamDataMode::Inline => {
                    let payload = stream_payload_with_decode_status(&stream, decode_level);
                    let dict = normalized_emitted_stream_dict(&stream, payload.decode_succeeded);
                    let ordered = ordered_qpdf_dict(pdf, &dict)?;
                    let bytes = payload.bytes.into_owned();
                    (
                        Some(Json::make_blob(move |sink| sink.write(&bytes))),
                        ordered,
                    )
                }
            };

            Json::write_dictionary_key(out, objects_first, key.as_bytes(), 3)?;
            let mut object_first = true;
            Json::write_dictionary_open(out, &mut object_first, 3)?;
            Json::write_dictionary_key(out, &mut object_first, b"stream", 4)?;
            let mut stream_first = true;
            Json::write_dictionary_open(out, &mut stream_first, 4)?;
            if let Some(data) = data {
                Json::write_dictionary_item(out, &mut stream_first, b"data", &data, 5)?;
            }
            Json::write_dictionary_key(out, &mut stream_first, b"dict", 5)?;
            dict.write(out, 5)?;
            Json::write_dictionary_close(out, stream_first, 4)?;
            Json::write_dictionary_close(out, object_first, 3)?;
        }
        return Ok(());
    }

    write_non_stream_value_entry(handle, key.as_bytes(), out, objects_first)?;
    Ok(())
}

fn write_file_mode_object_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    decode_level: DecodeLevel,
    prefix: &str,
    out: &mut dyn Pipeline,
    objects_first: &mut bool,
) -> Result<(), JsonOutputError> {
    let object_ref = handle
        .object_ref()
        .expect("qpdf object-map entries are indirect handles");
    let key = format!("obj:{} {} R", object_ref.number, object_ref.generation);

    handle.try_dereference().map_err(ConvertError::from)?;
    if handle.type_code() == 10 {
        // See the non-file writer: stream payload/datafile retry semantics are
        // intentionally left to the following stream consumer slice.
        let Object::Stream(stream) = qpdf_resolve_top_level_object(pdf, object_ref)? else {
            return Err(ConvertError::PdfError(
                "canonical stream handle has no legacy stream payload".to_string(),
            )
            .into());
        };
        {
            Json::write_dictionary_key(out, objects_first, key.as_bytes(), 3)?;
            let mut object_first = true;
            Json::write_dictionary_open(out, &mut object_first, 3)?;
            Json::write_dictionary_key(out, &mut object_first, b"stream", 4)?;

            let side_path = format_json_side_file_path(prefix, object_ref.number);
            let mut side_file = File::create(&side_path)
                .map_err(|source| side_file_io_error("open", &side_path, source))?;
            write_json_stream_file(pdf, &stream, decode_level, &side_path, &mut side_file, out)?;
            Json::write_dictionary_close(out, object_first, 3)?;
        }
        return Ok(());
    }

    write_non_stream_value_entry(handle, key.as_bytes(), out, objects_first)?;
    Ok(())
}

fn write_non_stream_value_entry(
    handle: &ObjectHandle,
    key: &[u8],
    out: &mut dyn Pipeline,
    objects_first: &mut bool,
) -> Result<(), JsonOutputError> {
    Json::write_dictionary_key(out, objects_first, key, 3)?;
    let mut object_first = true;
    Json::write_dictionary_open(out, &mut object_first, 3)?;
    Json::write_dictionary_key(out, &mut object_first, b"value", 4)?;
    handle.write_json(SUPPORTED_JSON_VERSION, out, true, 4)?;
    Json::write_dictionary_close(out, object_first, 3)?;
    Ok(())
}

/// Write one stream's payload to its side file and its JSON value to `out`.
///
/// The side file's terminal stage is finished explicitly before the file is
/// closed, matching qpdf's side-file lifecycle.
pub(crate) fn write_json_stream_file<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream: &Stream,
    decode_level: DecodeLevel,
    side_path: &str,
    side_file: &mut dyn Write,
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError> {
    let mut buffered = StdioBuffer::new(side_file);
    let mut terminal = PlStdioFile::new("stream data", &mut buffered);
    write_file_mode_stream_value(pdf, stream, decode_level, side_path, &mut terminal, out)?;
    terminal.finish()?;
    Ok(())
}

fn write_file_mode_stream_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream: &Stream,
    decode_level: DecodeLevel,
    side_path: &str,
    side_file: &mut dyn Pipeline,
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError> {
    let payload = stream_payload_with_decode_status(stream, decode_level);
    let mut stream_first = true;
    Json::write_dictionary_open(out, &mut stream_first, 4)?;
    Json::write_dictionary_item(
        out,
        &mut stream_first,
        b"datafile",
        &Json::make_string(side_path),
        5,
    )?;
    side_file.write(payload.bytes.as_ref())?;

    let dict = normalized_emitted_stream_dict(stream, payload.decode_succeeded);
    let dict_json = ordered_qpdf_dict(pdf, &dict)?;
    Json::write_dictionary_key(out, &mut stream_first, b"dict", 5)?;
    dict_json.write(out, 5)?;
    Json::write_dictionary_close(out, stream_first, 4)?;
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_handle::ObjectValue;
    use crate::pipeline::test_support::{RecordingSink, TraceCall};
    use crate::pipeline::PlString;
    use std::rc::Rc;

    fn load_one_page_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("../../tests/fixtures/compat/one-page.pdf");
        let bytes = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("one-page.pdf not found at {}: {e}", fixture.display()));
        Pdf::open_mem_owned(bytes).expect("failed to open one-page.pdf")
    }

    fn write_key(complete: bool, first_key: bool) -> (Vec<u8>, bool) {
        let mut pdf = load_one_page_pdf();
        let mut bytes = Vec::new();
        let mut first_key = first_key;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_json_key(
                &mut pdf,
                2,
                &mut out,
                complete,
                &mut first_key,
                DecodeLevel::None,
                &StreamDataMode::None,
                &[],
            )
            .expect("the qpdf key must be written");
        }
        (bytes, first_key)
    }

    #[test]
    fn incomplete_key_omits_the_separator_only_when_it_is_first() {
        let (first, first_key_after) = write_key(false, true);
        let (later, _) = write_key(false, false);

        assert!(first.starts_with(b"\n  \"qpdf\": ["), "{first:?}");
        assert!(later.starts_with(b",\n  \"qpdf\": ["), "{later:?}");
        // Only the separator differs; the key's value is identical.
        assert_eq!(first, later[1..]);
        // Neither form closes the caller's dictionary.
        assert!(first.ends_with(b"\n  ]"), "{first:?}");
        assert!(!first_key_after, "writing the key consumes the first slot");
    }

    #[test]
    fn complete_document_ignores_the_incoming_first_key_state() {
        let (from_first, first_key_after) = write_key(true, true);
        let (from_later, _) = write_key(true, false);

        assert_eq!(from_first, from_later);
        assert!(
            from_first.starts_with(b"{\n  \"qpdf\": ["),
            "{from_first:?}"
        );
        assert!(from_first.ends_with(b"\n  ]\n}\n"), "{from_first:?}");
        assert!(!first_key_after, "writing the key consumes the first slot");
    }

    #[test]
    fn complete_document_finishes_the_sink_and_the_key_form_does_not() {
        for complete in [true, false] {
            let mut pdf = load_one_page_pdf();
            let mut sink = RecordingSink::new(&[], &[]);
            let trace = sink.trace();
            let mut first_key = true;
            write_json_key(
                &mut pdf,
                2,
                &mut sink,
                complete,
                &mut first_key,
                DecodeLevel::None,
                &StreamDataMode::None,
                &[],
            )
            .expect("the qpdf key must be written");

            let finishes = trace
                .borrow()
                .calls
                .iter()
                .filter(|call| matches!(call, TraceCall::Finish { .. }))
                .count();
            assert_eq!(finishes, usize::from(complete), "complete={complete}");
        }
    }

    #[test]
    fn key_form_rejects_versions_other_than_two_before_writing() {
        let mut pdf = load_one_page_pdf();
        let mut bytes = Vec::new();
        let mut first_key = true;
        let result = {
            let mut out = PlString::new("json", None, &mut bytes);
            write_json_key(
                &mut pdf,
                3,
                &mut out,
                false,
                &mut first_key,
                DecodeLevel::None,
                &StreamDataMode::None,
                &[],
            )
        };

        assert!(matches!(result, Err(JsonOutputError::UnsupportedVersion)));
        assert!(bytes.is_empty());
        assert!(first_key, "a rejected version leaves the caller's state");
    }

    #[test]
    fn stream_consumer_rejects_a_legacy_value_that_disagrees_with_the_canonical_stream() {
        let mut pdf = load_one_page_pdf();
        let object_ref = ObjectRef::new(1, 0);
        pdf.set_object(object_ref, Object::Integer(42));
        let handle = pdf.get_object_handle(object_ref);
        handle.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            stream_length: 0,
        });

        let mut bytes = Vec::new();
        let mut out = PlString::new("json", None, &mut bytes);
        let mut objects_first = true;
        let non_file_error = write_non_file_mode_object_entry(
            &mut pdf,
            &handle,
            DecodeLevel::None,
            NonFileStreamDataMode::None,
            &mut out,
            &mut objects_first,
        )
        .expect_err("non-file stream bridge must reject the mismatched legacy value");
        assert!(matches!(
            non_file_error,
            JsonOutputError::Convert(ConvertError::PdfError(message))
                if message == "canonical stream handle has no legacy stream payload"
        ));

        let file_error = write_file_mode_object_entry(
            &mut pdf,
            &handle,
            DecodeLevel::None,
            "unused-prefix",
            &mut out,
            &mut objects_first,
        )
        .expect_err("file stream bridge must reject the mismatched legacy value");
        assert!(matches!(
            file_error,
            JsonOutputError::Convert(ConvertError::PdfError(message))
                if message == "canonical stream handle has no legacy stream payload"
        ));
    }
}
