//! qpdf correspondence: QPDF_json.cc output side — the free function `writeJSONStreamFile` and both `QPDF::writeJSON` overloads. The input-side `JSONReactor`, `createFromJSON`, `updateFromJSON`, and `importJSON` boundary lives in the private JSON document module (the output and input paths remain separate qpdf responsibilities).
//! Write a document's `qpdf` JSON v2 key.
//!
//! The `qpdf` key is a two-element array: a fixed metadata object followed by
//! the raw object map, in which every selected indirect object appears under an
//! `obj:N M R` key and the trailer under `trailer`. qpdf 11.9.0 keeps this
//! serialization separate from the section builders that assemble the
//! surrounding JSON document, and this module holds the same boundary; the
//! builders live in [`crate::json_inspect`].
//!
//! JSON v2 stream data is written through the `qpdf` key. JSON v1 keeps the
//! historical `objects` and `objectinfo` maps, which are implemented below
//! with the same canonical ObjectHandle writer.
//!
//! Per-object value serialization is not part of this boundary — qpdf
//! delegates it to `QPDFObjectHandle::writeJSON`, whose flpdf counterpart is
//! on [`ObjectHandle`]. Per-stream serialization is likewise delegated to the
//! canonical `ObjectHandle::write_stream_json` port of
//! `QPDF_Stream::writeStreamJSON` (`libqpdf/QPDF_Stream.cc:207-295`); this
//! module retains only object-map framing and side-file ownership.

use crate::json::Json;
use crate::json_inspect::{
    side_file_io_error, ConvertError, DecodeLevel, JsonObjectSelector, JsonOutputError,
    StreamDataMode,
};
use crate::object_handle::{ObjectHandle, QpdfStreamJsonData};
use crate::pipeline::buffer::Buffer;
use crate::pipeline::stdio_file::StdioBuffer;
use crate::pipeline::{Pipeline, PlStdioFile};
use crate::ObjectRef;
use crate::Pdf;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;

/// The only qpdf JSON version whose serialization is defined here.
const SUPPORTED_JSON_VERSION: i32 = 2;

/// Write qpdf JSON v1's top-level `objects` map into an already-open document.
///
/// qpdf writes object references in object-number order and appends the
/// trailer after the indirect objects (`QPDFJob.cc:958-980`). Object values
/// are delegated to `QPDFObjectHandle::writeJSON(1, ..., true)` so names,
/// strings, and stream dictionaries retain the v1 encoding.
pub(crate) fn write_json_v1_objects_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut dyn Pipeline,
    first: &mut bool,
    wanted_objects: &[JsonObjectSelector],
) -> Result<(), JsonOutputError> {
    Json::write_dictionary_key(out, first, b"objects", 1)?;
    let mut object_first = true;
    Json::write_dictionary_open(out, &mut object_first, 1)?;
    for handle in pdf.get_all_objects().map_err(ConvertError::from)? {
        let object_ref = handle
            .object_ref()
            .expect("qpdf object-map entries are indirect handles");
        if !object_selected(wanted_objects, object_ref) {
            continue;
        }
        let key = format!("{} {} R", object_ref.number, object_ref.generation);
        Json::write_dictionary_key(out, &mut object_first, key.as_bytes(), 2)?;
        handle.write_json(1, out, true, 2)?;
    }
    if trailer_selected(wanted_objects) {
        Json::write_dictionary_key(out, &mut object_first, b"trailer", 2)?;
        pdf.trailer().write_json(1, out, true, 2)?; // cov:ignore: llvm-cov attributes this successful trailer serialization to its opening write expressions
    } // cov:ignore: llvm-cov attributes the successful trailer branch continuation to its write expressions
    Json::write_dictionary_close(out, object_first, 1)?;
    Ok(())
}

/// Write qpdf JSON v1's top-level `objectinfo` map into an already-open
/// document (`QPDFJob.cc:1001-1027`).
pub(crate) fn write_json_v1_objectinfo_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut dyn Pipeline,
    first: &mut bool,
    wanted_objects: &[JsonObjectSelector],
) -> Result<(), JsonOutputError> {
    Json::write_dictionary_key(out, first, b"objectinfo", 1)?;
    let mut object_first = true;
    Json::write_dictionary_open(out, &mut object_first, 1)?;
    for handle in pdf.get_all_objects().map_err(ConvertError::from)? {
        let object_ref = handle
            .object_ref()
            .expect("qpdf object-map entries are indirect handles");
        if !object_selected(wanted_objects, object_ref) {
            continue;
        }

        let resolved = handle.clone();
        pdf.resolve(&resolved).map_err(ConvertError::from)?;
        let (is_stream, filter, length) = if let Some(stream_dict) = resolved
            .as_stream_dict()
            .and_then(|dict| dict.as_dictionary())
        {
            let filter = stream_dict
                .get(b"/Filter".as_slice())
                .cloned()
                .unwrap_or_else(ObjectHandle::null);
            let length = stream_dict
                .get(b"/Length".as_slice())
                .cloned()
                .unwrap_or_else(ObjectHandle::null);
            (true, filter, length)
        } else {
            (false, ObjectHandle::null(), ObjectHandle::null())
        };

        let key = format!("{} {} R", object_ref.number, object_ref.generation);
        Json::write_dictionary_key(out, &mut object_first, key.as_bytes(), 2)?;
        let mut details_first = true;
        Json::write_dictionary_open(out, &mut details_first, 2)?;
        Json::write_dictionary_key(out, &mut details_first, b"stream", 3)?;
        let mut stream_first = true;
        Json::write_dictionary_open(out, &mut stream_first, 3)?;
        Json::write_dictionary_key(out, &mut stream_first, b"filter", 4)?;
        filter.write_json(1, out, true, 4)?;
        Json::write_dictionary_item(
            out,
            &mut stream_first,
            b"is",
            &Json::make_bool(is_stream),
            4,
        )?; // cov:ignore: llvm-cov attributes this successful objectinfo field serialization to its opening write expressions
        Json::write_dictionary_key(out, &mut stream_first, b"length", 4)?;
        length.write_json(1, out, true, 4)?;
        Json::write_dictionary_close(out, stream_first, 3)?;
        Json::write_dictionary_close(out, details_first, 2)?;
    }
    Json::write_dictionary_close(out, object_first, 1)?;
    Ok(())
}

/// Format the side-file path for a `File`-mode stream entry.
///
/// qpdf 11.9.0 names side files `<prefix>-<obj_num>` — the bare object
/// number with no zero-padding. Centralized here so the JSON `datafile`
/// value and the side-file writer always produce the same name.
pub fn format_json_side_file_path(prefix: &[u8], obj_num: u32) -> Vec<u8> {
    let mut path = prefix.to_vec();
    path.push(b'-');
    path.extend_from_slice(obj_num.to_string().as_bytes());
    path
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
                QpdfStreamJsonData::None,
                out,
                &mut objects_first,
            ),
            StreamDataMode::Inline => write_non_file_mode_object_entry(
                pdf,
                &handle,
                decode_level,
                QpdfStreamJsonData::Inline,
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
        let trailer = pdf.trailer();
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

fn stream_decode_level(level: DecodeLevel) -> crate::writer::DecodeLevel {
    match level {
        DecodeLevel::None => crate::writer::DecodeLevel::None,
        DecodeLevel::Generalized => crate::writer::DecodeLevel::Generalized,
        DecodeLevel::Specialized => crate::writer::DecodeLevel::Specialized,
        DecodeLevel::All => crate::writer::DecodeLevel::All,
    }
}

fn write_non_file_mode_object_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    decode_level: DecodeLevel,
    stream_mode: QpdfStreamJsonData,
    out: &mut dyn Pipeline,
    objects_first: &mut bool,
) -> Result<(), JsonOutputError> {
    let object_ref = handle
        .object_ref()
        .expect("qpdf object-map entries are indirect handles");
    let key = format!("obj:{} {} R", object_ref.number, object_ref.generation);

    // Resolve the canonical object before dispatching by value. The object-map
    // entry itself remains keyed by its original indirect identity, while the
    // resolved handle supplies the live dictionary or stream value.
    let handle = pdf.resolve_handle(handle).map_err(ConvertError::from)?;
    if handle.type_code().map_err(ConvertError::from)? == 10 {
        // The former split consumer resolved and converted the complete
        // stream value before it wrote the object key. Keep that established
        // failure prefix while routing the conversion itself through the
        // canonical ObjectHandle writer.
        let mut stream_value = Buffer::new("stream JSON", None);
        handle.write_stream_json(
            SUPPORTED_JSON_VERSION,
            &mut stream_value,
            stream_mode,
            stream_decode_level(decode_level),
            None,
            "",
            false,
            4,
        )?;
        stream_value.finish()?;
        let stream_value = stream_value.take_buffer()?;

        Json::write_dictionary_key(out, objects_first, key.as_bytes(), 3)?;
        let mut object_first = true;
        Json::write_dictionary_open(out, &mut object_first, 3)?;
        Json::write_dictionary_key(out, &mut object_first, b"stream", 4)?;
        out.write(&stream_value)?;
        Json::write_dictionary_close(out, object_first, 3)?;
        return Ok(());
    }

    write_non_stream_value_entry(&handle, key.as_bytes(), out, objects_first)?;
    Ok(())
}

fn write_file_mode_object_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    decode_level: DecodeLevel,
    prefix: &[u8],
    out: &mut dyn Pipeline,
    objects_first: &mut bool,
) -> Result<(), JsonOutputError> {
    let object_ref = handle
        .object_ref()
        .expect("qpdf object-map entries are indirect handles");
    let key = format!("obj:{} {} R", object_ref.number, object_ref.generation);

    // Resolve the canonical object before dispatching by value, keeping the
    // original indirect identity for the JSON object key.
    let handle = pdf.resolve_handle(handle).map_err(ConvertError::from)?;
    if handle.type_code().map_err(ConvertError::from)? == 10 {
        Json::write_dictionary_key(out, objects_first, key.as_bytes(), 3)?;
        let mut object_first = true;
        Json::write_dictionary_open(out, &mut object_first, 3)?;
        Json::write_dictionary_key(out, &mut object_first, b"stream", 4)?;

        let side_path = format_json_side_file_path(prefix, object_ref.number);
        let side_path_fs = path_from_bytes(&side_path);
        let mut side_file = File::create(&side_path_fs).map_err(|source| {
            let rendered = String::from_utf8_lossy(&side_path);
            side_file_io_error("open", &rendered, source)
        })?;
        write_json_stream_file(&handle, decode_level, &side_path, &mut side_file, out)?;
        Json::write_dictionary_close(out, object_first, 3)?;
        return Ok(());
    }

    write_non_stream_value_entry(&handle, key.as_bytes(), out, objects_first)?;
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
pub(crate) fn write_json_stream_file(
    handle: &ObjectHandle,
    decode_level: DecodeLevel,
    side_path: &[u8],
    side_file: &mut dyn Write,
    out: &mut dyn Pipeline,
) -> Result<(), JsonOutputError> {
    let mut buffered = StdioBuffer::new(side_file);
    let mut terminal = PlStdioFile::new("stream data", &mut buffered);
    handle.write_stream_json(
        SUPPORTED_JSON_VERSION,
        out,
        QpdfStreamJsonData::File,
        stream_decode_level(decode_level),
        Some(&mut terminal),
        side_path,
        false,
        4,
    )?;
    terminal.finish()?;
    Ok(())
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }

    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────
