//! qpdf correspondence: `QPDF_json.cc` validators, deferred stream providers, and `JSONReactor::makeObject` value construction.
//! (`libqpdf/QPDF_json.cc:65-209, 212-231, 732-793`; `libqpdf/QUtil.cc:642-663`).
//!
//! This is the canonical value boundary for the JSON input importer. It builds
//! document-owned `ObjectHandle` values and never routes through the legacy
//! `Object`/`Pdf::set_object` representation.

use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::rc::Rc;

use super::value::format_qpdf_real;
use super::Json;
use crate::object_handle::{ObjectValue, StreamDataProvider};
use crate::pipeline::{Base64Action, Pipeline, PlBase64};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};

const STREAM_PROVIDER_BUFFER_SIZE: usize = 8192;

/// Parse qpdf's exact JSON indirect-reference spelling.
///
/// This intentionally accepts only ASCII spaces between the three tokens,
/// matching `QPDF_json.cc:is_indirect_object`: tabs and other JSON whitespace
/// are not accepted by this validator.
pub(crate) fn parse_indirect_reference(value: &[u8]) -> Option<ObjectRef> {
    let mut cursor = 0;
    let object_start = cursor;
    while value.get(cursor).is_some_and(|byte| byte.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == object_start || value.get(cursor) != Some(&b' ') {
        return None;
    }
    let object = std::str::from_utf8(&value[object_start..cursor])
        .ok()?
        .parse::<u64>()
        .ok()?;

    while value.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    let generation_start = cursor;
    while value.get(cursor).is_some_and(|byte| byte.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == generation_start || value.get(cursor) != Some(&b' ') {
        return None;
    }
    let generation = std::str::from_utf8(&value[generation_start..cursor])
        .ok()?
        .parse::<u64>()
        .ok()?;

    while value.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    if value.get(cursor) != Some(&b'R') || cursor + 1 != value.len() {
        return None;
    }

    if object == 0 {
        return None;
    }
    Some(ObjectRef::new(
        u32::try_from(object).ok()?,
        u16::try_from(generation).ok()?,
    ))
}

/// Parse qpdf's `obj:N G R` object-key spelling.
pub(crate) fn parse_object_key(value: &[u8]) -> Option<ObjectRef> {
    value
        .strip_prefix(b"obj:")
        .and_then(parse_indirect_reference)
}

/// Create qpdf's lazy provider for a JSON `stream.data` string.
///
/// This ports `QPDF_json.cc:212-231`. The parser records a string's source
/// extent including its quotes, so the provider seeks to `start + 1` and
/// reads through `end - 1` only when the stream is piped. The source bytes
/// are fed incrementally to qpdf's Base64 decoder; decoded data is never
/// materialized in a `Vec`.
pub(crate) fn inline_stream_data_provider<R: Read + Seek + 'static>(
    source: Rc<RefCell<R>>,
    value: &Json,
) -> Result<Rc<dyn StreamDataProvider>> {
    let (start, length) = inline_data_range(value)?;

    Ok(Rc::new(InlineStreamDataProvider {
        source,
        start,
        length,
    }))
}

struct InlineStreamDataProvider<R: Read + Seek + 'static> {
    source: Rc<RefCell<R>>,
    start: u64,
    length: u64,
}

impl<R: Read + Seek + 'static> StreamDataProvider for InlineStreamDataProvider<R> {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        let mut decode = PlBase64::new("base64-decode", pipeline, Base64Action::Decode);
        let mut source = self.source.borrow_mut();
        source.seek(SeekFrom::Start(self.start))?;
        let mut remaining = self.length;
        let mut buffer = [0_u8; STREAM_PROVIDER_BUFFER_SIZE];
        while remaining > 0 {
            let requested = remaining.min(STREAM_PROVIDER_BUFFER_SIZE as u64) as usize;
            let len = loop {
                match source.read(&mut buffer[..requested]) {
                    Ok(len) => break len,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                }
            };
            if len == 0 {
                break;
            }
            decode.write(&buffer[..len]).map_err(Error::from)?;
            remaining -= len as u64;
        }
        decode.finish().map_err(Error::from)
    }
}

fn inline_data_range(value: &Json) -> Result<(u64, u64)> {
    let start = value
        .start()
        .checked_add(1)
        .ok_or_else(|| Error::Internal("QPDF_json: JSON string start overflow".into()))?;
    let end = value
        .end()
        .checked_sub(1)
        .ok_or_else(|| Error::Internal("QPDF_json: JSON string end underflow".into()))?;
    if end < start {
        return Err(Error::Internal(
            "QPDF_json: JSON string length < 0".to_owned(),
        ));
    }
    let length = end
        .checked_sub(start)
        .ok_or_else(|| Error::Internal("QPDF_json: JSON string length is out of range".into()))?;
    let length = u64::try_from(length)
        .map_err(|_| Error::Internal("QPDF_json: JSON string length is out of range".into()))?;
    let start = u64::try_from(start)
        .map_err(|_| Error::Internal("QPDF_json: JSON string start is negative".into()))?;
    Ok((start, length))
}

/// Create qpdf's lazy provider for a JSON `stream.datafile` value.
///
/// This ports `QUtil::file_provider` (`libqpdf/QUtil.cc:642-663`). Opening
/// the named file is deliberately inside the callback, so registration never
/// touches the filesystem. Each invocation opens and streams the file again,
/// preserving qpdf's repeatable provider boundary while the file is stable.
pub(crate) fn datafile_stream_data_provider(
    filename: impl Into<PathBuf>,
) -> Rc<dyn StreamDataProvider> {
    Rc::new(DatafileStreamDataProvider {
        filename: filename.into(),
    })
}

struct DatafileStreamDataProvider {
    filename: PathBuf,
}

impl StreamDataProvider for DatafileStreamDataProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        let mut file = File::open(&self.filename)
            .map_err(|error| Error::System(format!("open {}: {error}", self.filename.display())))?;
        let mut buffer = [0_u8; STREAM_PROVIDER_BUFFER_SIZE];
        loop {
            let read = loop {
                match file.read(&mut buffer) {
                    Ok(read) => break Ok(read),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => break Err(error),
                }
            };
            match read {
                Ok(0) => break,
                Ok(len) => pipeline.write(&buffer[..len]).map_err(Error::from)?,
                Err(_error) => {
                    pipeline.finish().map_err(Error::from)?;
                    return Err(Error::System(format!(
                        "failure reading file {}",
                        self.filename.display()
                    )));
                }
            }
        }
        pipeline.finish().map_err(Error::from)
    }
}

/// Convert one JSON value to a canonical document-owned handle.
pub(crate) fn json_value_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    value: &Json,
) -> Result<ObjectHandle> {
    if value.is_dictionary() {
        let mut entries = std::collections::BTreeMap::new();
        let mut previous_key = None;
        while let Some((raw_key, child)) = value.next_dictionary_item_after(previous_key.as_deref())
        {
            let key = json_dictionary_key(&raw_key)?;
            entries.insert(key, json_value_to_handle(pdf, &child)?);
            previous_key = Some(raw_key);
        }
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Dictionary(entries)));
    }

    if value.is_array() {
        let children = value
            .array_items_snapshot()
            .ok_or_else(|| Error::Internal("JSON array disappeared during conversion".into()))?
            .iter()
            .map(|child| json_value_to_handle(pdf, child))
            .collect::<Result<Vec<_>>>()?;
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Array(children)));
    }

    if value.is_null() {
        return Ok(pdf.resolver.direct_object_handle(ObjectValue::Null));
    }
    if let Some(boolean) = value.get_bool() {
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Boolean(boolean)));
    }
    if let Some(number) = value.get_number() {
        return json_number_to_handle(pdf, &number);
    }
    if let Some(string) = value.get_string() {
        return json_string_to_handle(pdf, &string);
    }

    Err(Error::Internal(
        "JSON value has no initialized qpdf value kind".into(),
    ))
}

fn json_number_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    number: &[u8],
) -> Result<ObjectHandle> {
    let text = std::str::from_utf8(number)
        .map_err(|_| Error::Unsupported("invalid JSON number".into()))?;
    if let Ok(integer) = text.parse::<i64>() {
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Integer(integer)));
    }

    let real = text
        .parse::<f64>()
        .map_err(|_| Error::Unsupported("invalid JSON number".into()))?;
    if !real.is_finite() {
        return Err(Error::Unsupported("invalid JSON number".into()));
    }
    let literal = if text.contains(['e', 'E']) {
        format_qpdf_real(real)
    } else {
        text.to_owned()
    };
    Ok(pdf.resolver.direct_object_handle(ObjectValue::RealLiteral {
        value: real,
        literal: literal.into_bytes(),
    }))
}

fn json_string_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    string: &[u8],
) -> Result<ObjectHandle> {
    if let Some(object_ref) = parse_indirect_reference(string) {
        return Ok(pdf.get_object_handle(object_ref));
    }

    if let Some(unicode) = string.strip_prefix(b"u:") {
        return Ok(pdf.resolver.direct_object_handle(ObjectValue::String(
            crate::pdf_string::new_unicode_string(unicode),
        )));
    }

    if let Some(binary) = string.strip_prefix(b"b:") {
        if binary.len() % 2 != 0 || !binary.iter().all(u8::is_ascii_hexdigit) {
            return Ok(pdf.resolver.direct_object_handle(ObjectValue::Null));
        }
        let decoded = hex::decode(binary)
            .map_err(|_| Error::Unsupported("invalid binary JSON string".into()))?;
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::String(decoded)));
    }

    if string.first() == Some(&b'/') && string.len() > 1 {
        return Ok(pdf
            .resolver
            .direct_object_handle(ObjectValue::Name(string[1..].to_vec())));
    }

    if string.starts_with(b"n:/") && string.len() > 3 {
        let parsed = ObjectHandle::parse(&string[2..])?;
        let name = parsed
            .as_name()
            .ok_or_else(|| Error::Internal("PDF name parser returned a non-name".into()))?;
        return Ok(pdf.resolver.direct_object_handle(ObjectValue::Name(name)));
    }

    // QPDF_json.cc reports an error and substitutes null for an unrecognized
    // string. The Reactor owns warning/error attribution; this value factory
    // supplies the same null replacement without emitting a second warning.
    Ok(pdf.resolver.direct_object_handle(ObjectValue::Null))
}

fn json_dictionary_key(key: &[u8]) -> Result<Vec<u8>> {
    if key.starts_with(b"n:/") && key.len() > 3 {
        let parsed = ObjectHandle::parse(&key[2..])?;
        let name = parsed.as_name().ok_or_else(|| {
            // cov:ignore-start: a successful parse of slash-prefixed input cannot yield a non-name
            Error::Internal("PDF dictionary key parser returned a non-name".into())
            // cov:ignore-end
        })?; // cov:ignore: same parser invariant
        let mut canonical = Vec::with_capacity(name.len() + 1);
        canonical.push(b'/');
        canonical.extend_from_slice(&name);
        return Ok(canonical);
    }
    Ok(crate::object_handle::canonical_dictionary_key(key))
}
