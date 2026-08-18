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
//! Only JSON version 2 is supported. Any other version is rejected with
//! [`JsonOutputError::UnsupportedVersion`].
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
use crate::object::ObjectRef;
use crate::object_handle::{ObjectHandle, QpdfStreamJsonData};
use crate::pipeline::buffer::Buffer;
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

    // Chase a `Pdf::set_object`-installed bare-reference redirect
    // (`ObjectValue::Reference`) to its terminal value before dispatching.
    // The replaced `qpdf_resolve_top_level_object` path did this chase
    // itself; `ObjectHandle::write_json`'s `dereference_indirect` only
    // dereferences `handle`'s own indirect identity (one hop), so a
    // resolved value that is itself a bare reference needs this second,
    // explicit chase or the entry both serializes the wrong value and
    // (for a redirect-to-stream) is misrouted below, since `type_code()`
    // on the un-chased holder reports 13 (unresolved/reference), never 10.
    let handle = pdf
        .resolve_object_handle_to_terminal(handle)
        .map_err(ConvertError::from)?;
    if handle.type_code() == 10 {
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
    prefix: &str,
    out: &mut dyn Pipeline,
    objects_first: &mut bool,
) -> Result<(), JsonOutputError> {
    let object_ref = handle
        .object_ref()
        .expect("qpdf object-map entries are indirect handles");
    let key = format!("obj:{} {} R", object_ref.number, object_ref.generation);

    // See the non-file writer: chase a `Pdf::set_object` bare-reference
    // redirect to its terminal value before dispatching.
    let handle = pdf
        .resolve_object_handle_to_terminal(handle)
        .map_err(ConvertError::from)?;
    if handle.type_code() == 10 {
        Json::write_dictionary_key(out, objects_first, key.as_bytes(), 3)?;
        let mut object_first = true;
        Json::write_dictionary_open(out, &mut object_first, 3)?;
        Json::write_dictionary_key(out, &mut object_first, b"stream", 4)?;

        let side_path = format_json_side_file_path(prefix, object_ref.number);
        let mut side_file = File::create(&side_path)
            .map_err(|source| side_file_io_error("open", &side_path, source))?;
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
    side_path: &str,
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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::{RecordingSink, TraceCall};
    use crate::pipeline::PlString;
    use crate::{Dictionary, Object, Stream};

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
    fn stream_decode_level_maps_all_qpdf_levels() {
        assert_eq!(
            stream_decode_level(DecodeLevel::None),
            crate::writer::DecodeLevel::None
        );
        assert_eq!(
            stream_decode_level(DecodeLevel::Generalized),
            crate::writer::DecodeLevel::Generalized
        );
        assert_eq!(
            stream_decode_level(DecodeLevel::Specialized),
            crate::writer::DecodeLevel::Specialized
        );
        assert_eq!(
            stream_decode_level(DecodeLevel::All),
            crate::writer::DecodeLevel::All
        );
    }

    #[test]
    fn non_file_stream_conversion_failure_precedes_object_key() {
        let mut pdf = load_one_page_pdf();
        let mut stream_dict = Dictionary::new();
        stream_dict.insert(b"/Bad", Object::Real(f64::NAN));
        let object_ref = ObjectRef::new(7, 0);
        pdf.set_object(
            object_ref,
            Object::Stream(Stream::new(stream_dict, Vec::new())),
        );
        let handle = pdf.get_object_handle(object_ref);
        let mut bytes = Vec::new();
        let mut output = PlString::new("json", None, &mut bytes);
        let mut objects_first = true;

        let error = write_non_file_mode_object_entry(
            &mut pdf,
            &handle,
            DecodeLevel::None,
            QpdfStreamJsonData::None,
            &mut output,
            &mut objects_first,
        )
        .expect_err("non-finite stream dictionary values must fail conversion");

        assert!(matches!(
            error,
            JsonOutputError::Convert(ConvertError::NonFiniteFloat)
        ));
        assert!(bytes.is_empty());
        assert!(objects_first);
    }

    /// qpdf's replaced `qpdf_resolve_top_level_object` chased a
    /// [`Pdf::set_object`]-installed bare-reference redirect
    /// (`ObjectValue::Reference`, this crate's compatibility bridge for
    /// `Pdf::set_object(holder, Object::Reference(target))`) to its terminal
    /// value before writing the entry. `ObjectHandle::write_json`'s
    /// `dereference_indirect` only dereferences the holder's own indirect
    /// identity (one hop); a resolved value that is itself a bare reference
    /// needs a second, explicit chase (`Pdf::resolve_object_handle_to_terminal`,
    /// the same helper `outline_document_helper.rs`'s `resolve_value_handle`
    /// uses for this exact bridge shape) or the holder serializes as the
    /// literal `"target R"` string instead of the target's value.
    #[test]
    fn non_stream_entry_chases_a_set_object_reference_redirect_to_its_terminal_value() {
        let mut pdf = load_one_page_pdf();
        let target_ref = ObjectRef::new(50, 0);
        pdf.set_object(target_ref, Object::Integer(42));
        let holder_ref = ObjectRef::new(51, 0);
        pdf.set_object(holder_ref, Object::Reference(target_ref));
        let handle = pdf.get_object_handle(holder_ref);

        let mut bytes = Vec::new();
        let mut objects_first = true;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_non_file_mode_object_entry(
                &mut pdf,
                &handle,
                DecodeLevel::None,
                QpdfStreamJsonData::None,
                &mut out,
                &mut objects_first,
            )
            .expect("redirect chase must succeed");
        }
        let text = String::from_utf8(bytes).expect("json output must be utf8");
        assert!(
            text.contains("\"value\": 42"),
            "expected the terminal value 42, got {text}"
        );
        assert!(
            !text.contains("50 0 R"),
            "the redirect target must not surface as a literal reference: {text}"
        );
    }

    /// Same bridge as above, but the redirect's terminal value is itself a
    /// stream. The replaced path's chase also covered this case (the finding's
    /// own regression claim: "misclassifies redirects to streams"); the fix
    /// must gate the stream-vs-value dispatch on the chased terminal handle,
    /// not the un-chased holder, or the entry silently loses the stream
    /// wrapper, payload, and side file.
    #[test]
    fn non_file_stream_entry_chases_a_set_object_reference_redirect_to_a_stream() {
        let mut pdf = load_one_page_pdf();
        let target_ref = ObjectRef::new(50, 0);
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"Test".to_vec()));
        pdf.set_object(
            target_ref,
            Object::Stream(Stream::new(dict, b"hi".to_vec())),
        );
        let holder_ref = ObjectRef::new(51, 0);
        pdf.set_object(holder_ref, Object::Reference(target_ref));
        let handle = pdf.get_object_handle(holder_ref);

        let mut bytes = Vec::new();
        let mut objects_first = true;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_non_file_mode_object_entry(
                &mut pdf,
                &handle,
                DecodeLevel::None,
                QpdfStreamJsonData::Inline,
                &mut out,
                &mut objects_first,
            )
            .expect("redirect-to-stream chase must succeed");
        }
        let text = String::from_utf8(bytes).expect("json output must be utf8");
        assert!(
            text.contains("\"stream\""),
            "expected the stream wrapper, got {text}"
        );
        assert!(
            text.contains("/Type"),
            "expected the target stream's own dict, got {text}"
        );
        assert!(
            !text.contains("50 0 R"),
            "the redirect target must not surface as a literal reference: {text}"
        );
    }

    /// File-mode variant of the redirect-to-stream case above: the side file
    /// and `datafile` entry must belong to the chased terminal stream.
    #[test]
    fn file_mode_stream_entry_chases_a_set_object_reference_redirect_to_a_stream() {
        let mut pdf = load_one_page_pdf();
        let target_ref = ObjectRef::new(50, 0);
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"Test".to_vec()));
        pdf.set_object(
            target_ref,
            Object::Stream(Stream::new(dict, b"hi".to_vec())),
        );
        let holder_ref = ObjectRef::new(51, 0);
        pdf.set_object(holder_ref, Object::Reference(target_ref));
        let handle = pdf.get_object_handle(holder_ref);

        let dir = std::env::temp_dir();
        let prefix = dir.join(format!(
            "flpdf-document-json-redirect-stream-file-{}",
            std::process::id()
        ));
        let prefix = prefix.to_str().expect("prefix must be valid utf8");

        let mut bytes = Vec::new();
        let mut objects_first = true;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_file_mode_object_entry(
                &mut pdf,
                &handle,
                DecodeLevel::None,
                prefix,
                &mut out,
                &mut objects_first,
            )
            .expect("redirect-to-stream chase must succeed");
        }
        let text = String::from_utf8(bytes).expect("json output must be utf8");
        assert!(
            text.contains("\"datafile\""),
            "expected the side-file datafile entry, got {text}"
        );
        // The side file is keyed by the enumerated top-level object
        // (`holder_ref`), the same as the ordinary non-redirect case, not by
        // the terminal stream's own object number: qpdf itself can never
        // hold this bridge shape (`QPDF::replaceObject` rejects an indirect
        // handle, `libqpdf/QPDF.cc:1980-1991`), so there is no qpdf
        // precedent for the redirect case specifically, and the enumerated
        // slot's number is what names "this JSON object's side file" in
        // every qpdf-reachable (non-redirect) case.
        let side_path = format_json_side_file_path(prefix, holder_ref.number);
        let side_data = std::fs::read(&side_path).expect("side file must be written");
        assert_eq!(side_data, b"hi");
        let _ = std::fs::remove_file(&side_path);
        assert!(
            !text.contains("50 0 R"),
            "the redirect target must not surface as a literal reference: {text}"
        );
    }

    /// Documents (does not fix) the canonical array writer's behavior for a
    /// nested reference whose generation is 65535, matching qpdf's own
    /// `writeJSON`: `QPDF_Array::writeJSON` (`QPDF_Array.cc:153-187`) decides
    /// reference-vs-value purely via `og.isIndirect()`
    /// (`QPDFObjGen::isIndirect()`, `include/qpdf/QPDFObjGen.hh`, defined as
    /// `obj != 0` with no generation check at all) — it never validates the
    /// generation. qpdf's generation-65535 rejection
    /// (`QPDFParser.cc:168-176`: `id < 1 || gen < 0 || gen >= 65535` =>
    /// `addNull()`) is a parse-time token-interpretation rule owned by the
    /// parser, not a property the object model or its JSON writer enforce.
    /// A live indirect handle with generation 65535 is reachable, in both
    /// qpdf and flpdf, only via programmatic construction that bypasses the
    /// parser (`flpdf`'s own `parser.rs:763-768` applies the identical
    /// `number >= 1 && (0..65535).contains(&generation)` filter at parse
    /// time), and on that path real qpdf writes the literal reference
    /// string, not `null`. Live-probed against `/usr/bin/qpdf` 11.9.0: a
    /// real PDF file with `[0 0 R 1 65535 R (marker)]` already collapses to
    /// `[null, null, "u:marker"]` before any writer runs (parser-owned), so
    /// the replaced `ordered_qpdf_object`/`reference_is_valid` null
    /// normalization this finding asks to restore was never exercised by a
    /// parsed document and is not qpdf's own array-writer behavior for the
    /// only way this shape can otherwise arise.
    #[test]
    fn nested_reference_with_generation_65535_matches_qpdf_array_writer_no_generation_check() {
        let mut pdf = load_one_page_pdf();
        let invalid_ref = ObjectRef::new(7, 65535);
        let mut inner = Dictionary::new();
        inner.insert(
            "Nested",
            Object::Array(vec![Object::Reference(invalid_ref)]),
        );
        let holder_ref = ObjectRef::new(60, 0);
        pdf.set_object(holder_ref, Object::Dictionary(inner));
        let handle = pdf.get_object_handle(holder_ref);

        let mut bytes = Vec::new();
        let mut objects_first = true;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_non_file_mode_object_entry(
                &mut pdf,
                &handle,
                DecodeLevel::None,
                QpdfStreamJsonData::None,
                &mut out,
                &mut objects_first,
            )
            .expect("a nested reference, valid or not, must not error");
        }
        let text = String::from_utf8(bytes).expect("json output must be utf8");
        assert!(
            text.contains("\"7 65535 R\""),
            "qpdf's array writer has no generation validity check \
             (QPDF_Array.cc:153-187 only tests isIndirect()); expected the \
             literal reference, got {text}"
        );
    }
    /// Guards the fix's new gate (`terminal.type_code() == 10`, checked
    /// after the canonical chase, but the stream branch body still reads
    /// the *legacy* representation via `qpdf_resolve_top_level_object`)
    /// against the two representations disagreeing about redirect-to-stream
    /// status. `Pdf::set_object`'s bounded lift
    /// (`Pdf::lift_for_set_object`, `reader.rs`) can fail for a value nested
    /// past `MAX_INLINE_DEPTH`, in which case it leaves the *canonical*
    /// handle graph untouched while the *legacy* cache still receives the
    /// full value (`reader.rs`'s `set_object` doc: "store `object` directly
    /// as the bridge's authoritative materialized value instead"). This
    /// constructs exactly that split — target's dict entry nested
    /// `MAX_INLINE_DEPTH + 5` deep — and confirms the canonical terminal
    /// chase reports `null` (not `10`/stream) for the un-lifted target, so
    /// the new gate correctly stays out of the stream branch instead of
    /// entering it and hitting the legacy/canonical mismatch error.
    #[test]
    fn redirect_to_a_stream_whose_lift_failed_falls_through_to_null_not_an_error() {
        let mut pdf = load_one_page_pdf();
        let target_ref = ObjectRef::new(50, 0);

        fn nest(depth: usize) -> Object {
            if depth == 0 {
                Object::Integer(1)
            } else {
                Object::Array(vec![nest(depth - 1)])
            }
        }
        let mut dict = Dictionary::new();
        dict.insert("Deep", nest(crate::object::MAX_INLINE_DEPTH + 5));
        pdf.set_object(
            target_ref,
            Object::Stream(Stream::new(dict, b"hi".to_vec())),
        );

        let holder_ref = ObjectRef::new(51, 0);
        pdf.set_object(holder_ref, Object::Reference(target_ref));
        let handle = pdf.get_object_handle(holder_ref);

        let terminal = pdf
            .resolve_object_handle_to_terminal(&handle)
            .expect("terminal chase must not error even when the lift failed");
        assert_eq!(
            terminal.type_code(),
            2,
            "an un-lifted target must not report as a stream to the new gate"
        );

        let mut bytes = Vec::new();
        let mut objects_first = true;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_non_file_mode_object_entry(
                &mut pdf,
                &handle,
                DecodeLevel::None,
                QpdfStreamJsonData::None,
                &mut out,
                &mut objects_first,
            )
            .expect("a lift failure must not surface as a canonical/legacy mismatch error");
        }
        let text = String::from_utf8(bytes).expect("json output must be utf8");
        assert!(
            text.contains("\"value\": null"),
            "expected the fallback null value, got {text}"
        );
    }

    /// The realistic counterpart of the redirect-to-stream tests above: the
    /// target is a real, file-parsed stream (never touched by
    /// `Pdf::set_object`) rather than one constructed in-memory. Confirms
    /// the canonical terminal chase and the legacy
    /// `qpdf_resolve_top_level_object` chase agree for ordinary parsed
    /// content, so the redirect-to-stream entry writes the full wrapper
    /// (data, dict) with no divergence error.
    #[test]
    fn redirect_to_a_real_preexisting_file_stream_writes_the_full_wrapper() {
        let mut pdf = load_one_page_pdf();
        let target_ref = ObjectRef::new(7, 0); // the fixture's own content stream
        let holder_ref = ObjectRef::new(51, 0);
        pdf.set_object(holder_ref, Object::Reference(target_ref));
        let handle = pdf.get_object_handle(holder_ref);

        let mut bytes = Vec::new();
        let mut objects_first = true;
        {
            let mut out = PlString::new("json", None, &mut bytes);
            write_non_file_mode_object_entry(
                &mut pdf,
                &handle,
                DecodeLevel::None,
                QpdfStreamJsonData::Inline,
                &mut out,
                &mut objects_first,
            )
            .expect("redirect to a real pre-existing stream must not error");
        }
        let text = String::from_utf8(bytes).expect("json output must be utf8");
        assert!(
            text.contains("\"data\""),
            "expected inline data, got {text}"
        );
        assert!(
            text.contains("\"dict\""),
            "expected the stream dict, got {text}"
        );
    }
}
