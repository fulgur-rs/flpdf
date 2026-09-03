//! qpdf correspondence: `QPDF_json.cc` JSONReactor state machine, validators, deferred stream providers, and `makeObject` value construction.
//! (`libqpdf/QPDF_json.cc:233-832`; `libqpdf/QUtil.cc:642-663`).
//!
//! This is the canonical value boundary for the JSON input importer. It builds
//! document-owned `ObjectHandle` values directly in the live document graph.
//!
//! The importer has two intentional category (B) internal substitutions whose
//! observable JSON contract is pinned to qpdf:
//!
//! - `validate_pdf_version` is a Rust byte-slice implementation of
//!   `QPDF::validatePDFVersion` (`libqpdf/QPDF.cc:366-384`), with the caller's
//!   full-consumption check from `QPDF_json.cc:503-518` preserved.
//! - `JsonDescription` is constructed per handle by
//!   `ObjectHandle::set_description_json`, instead of mutating qpdf's shared
//!   `QPDFValue::Description` in `QPDF_json.cc:721-730`; input name, object
//!   identity, and parsed offset remain the same.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::rc::Rc;

use super::value::format_qpdf_real;
use super::{Json, Reactor};
use crate::filespec_helper::qpdf_style_open_error;
use crate::object_handle::{ObjectValue, StreamDataProvider};
use crate::pipeline::{Base64Action, Pipeline, PlBase64};
use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};

const STREAM_PROVIDER_BUFFER_SIZE: usize = 8192;

/// Outcome of matching qpdf's `N G R` indirect-reference shape
/// (`is_indirect_object`, `libqpdf/QPDF_json.cc:66-104`).
///
/// Once the digit/space/`R` shape matches, qpdf unconditionally calls
/// `QUtil::string_to_int` on both the object and generation digit runs.
/// That conversion can itself overflow and throw (see
/// [`qpdf_string_to_int_checked`]), which is a fatal error, not "this
/// string isn't an indirect reference" -- callers must not collapse
/// [`Overflow`](IndirectReferenceParse::Overflow) into
/// [`NoMatch`](IndirectReferenceParse::NoMatch).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IndirectReferenceParse {
    NoMatch,
    Overflow(String),
    Reference(ObjectRef),
}

/// Parse qpdf's exact JSON indirect-reference spelling.
///
/// This intentionally accepts only ASCII spaces between the three tokens,
/// matching `QPDF_json.cc:is_indirect_object`: tabs and other JSON whitespace
/// are not accepted by this validator.
pub(crate) fn parse_indirect_reference(value: &[u8]) -> IndirectReferenceParse {
    let mut cursor = 0;
    let object_start = cursor;
    while value.get(cursor).is_some_and(|byte| byte.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == object_start || value.get(cursor) != Some(&b' ') {
        return IndirectReferenceParse::NoMatch;
    }
    // The loop above only ever advances `cursor` past bytes matching
    // `is_ascii_digit()`, so this slice is always valid single-byte UTF-8.
    let object_text =
        std::str::from_utf8(&value[object_start..cursor]).expect("ascii digit run is valid UTF-8");

    while value.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    let generation_start = cursor;
    while value.get(cursor).is_some_and(|byte| byte.is_ascii_digit()) {
        cursor += 1;
    }
    if cursor == generation_start || value.get(cursor) != Some(&b' ') {
        return IndirectReferenceParse::NoMatch;
    }
    let generation_text = std::str::from_utf8(&value[generation_start..cursor])
        .expect("ascii digit run is valid UTF-8");

    while value.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    if value.get(cursor) != Some(&b'R') || cursor + 1 != value.len() {
        return IndirectReferenceParse::NoMatch;
    }

    let object = match qpdf_string_to_int_checked(object_text) {
        QpdfIntParse::Overflow(message) => return IndirectReferenceParse::Overflow(message),
        QpdfIntParse::Value(value) => value,
        // `object_text` is a non-empty digit run (checked above), so
        // `qpdf_string_to_int_checked` can never report `NoDigits` here.
        QpdfIntParse::NoDigits => return IndirectReferenceParse::NoMatch, // cov:ignore: unreachable given the digit-run precondition above
    };
    let generation = match qpdf_string_to_int_checked(generation_text) {
        QpdfIntParse::Overflow(message) => return IndirectReferenceParse::Overflow(message),
        QpdfIntParse::Value(value) => value,
        // Same precondition as the object number, one line up.
        QpdfIntParse::NoDigits => return IndirectReferenceParse::NoMatch, // cov:ignore: unreachable given the digit-run precondition above
    };

    if object == 0 {
        return IndirectReferenceParse::NoMatch;
    }
    let (Ok(object), Ok(generation)) = (u32::try_from(object), u16::try_from(generation)) else {
        return IndirectReferenceParse::NoMatch;
    };
    IndirectReferenceParse::Reference(ObjectRef::new(object, generation))
}

/// Parse qpdf's `obj:N G R` object-key spelling.
pub(crate) fn parse_object_key(value: &[u8]) -> IndirectReferenceParse {
    match value.strip_prefix(b"obj:") {
        Some(rest) => parse_indirect_reference(rest),
        None => IndirectReferenceParse::NoMatch,
    }
}

/// Create qpdf's lazy provider for a JSON `stream.data` string.
///
/// This ports `QPDF_json.cc:212-231`. The parser records a string's source
/// extent including its quotes, so the provider seeks to `start + 1` and
/// reads through `end - 1` only when the stream is piped. The source bytes
/// are fed incrementally to qpdf's Base64 decoder; decoded data is never
/// materialized in a `Vec`.
///
/// `base_offset` is the source's own stream position when parsing began: the
/// tokenizer tracks offsets from its own first byte (`Parser::pos` starts at
/// `0`), not from the source's absolute position, so a caller-supplied
/// reader that was not already at the start needs this correction before the
/// provider seeks back into `source` later. qpdf's `InputSource` abstraction
/// (`libqpdf/QPDF_json.cc:212-230` reads through `is`) does not have this gap
/// because every `InputSource` implementation normalizes its own offset `0`
/// to its logical start; flpdf's `Read + Seek` substitute for `InputSource`
/// has no such normalization, so the caller boundary (`json/document.rs`)
/// supplies it explicitly instead.
pub(crate) fn inline_stream_data_provider<R: Read + Seek + 'static>(
    source: Rc<RefCell<R>>,
    value: &Json,
    base_offset: u64,
) -> Result<Rc<dyn StreamDataProvider>> {
    let (start, length) = inline_data_range(value, base_offset)?;

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

fn inline_data_range(value: &Json, base_offset: u64) -> Result<(u64, u64)> {
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
    let start = start
        .checked_add(base_offset)
        .ok_or_else(|| Error::Internal("QPDF_json: JSON string start overflow".into()))?;
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
            .map_err(|error| qpdf_style_open_error(&self.filename, error))?;
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

    // qpdf's real branch (QPDF_json.cc:750-764) reformats scientific
    // notation through std::stod, but a stod failure -- including overflow
    // to infinity -- is caught and the original text is kept unchanged:
    // qpdf never rejects a syntactically valid JSON number here. A
    // non-scientific literal is never even attempted: its original text
    // becomes the Real's value verbatim, regardless of magnitude.
    let literal = if text.contains(['e', 'E']) {
        match text.parse::<f64>() {
            Ok(value) if value.is_finite() => format_qpdf_real(value),
            _ => text.to_owned(),
        }
    } else {
        text.to_owned()
    };
    let value = text.parse::<f64>().unwrap_or(f64::NAN);
    Ok(pdf.resolver.direct_object_handle(ObjectValue::RealLiteral {
        value,
        literal: literal.into_bytes(),
    }))
}

fn json_string_to_handle<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    string: &[u8],
) -> Result<ObjectHandle> {
    match parse_indirect_reference(string) {
        IndirectReferenceParse::Reference(object_ref) => {
            return Ok(pdf.resolver.reserve_object_if_not_exists(object_ref));
        }
        IndirectReferenceParse::Overflow(message) => return Err(Error::System(message)),
        IndirectReferenceParse::NoMatch => {}
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReactorState {
    Top,
    Qpdf,
    QpdfMeta,
    Objects,
    Trailer,
    ObjectTop,
    Stream,
    Object,
    Ignore,
}

struct StackFrame {
    state: ReactorState,
    object: Option<ObjectHandle>,
}

/// Incremental qpdf JSON v2 importer state machine.
///
/// The parser calls dictionary/array item callbacks before it starts the child
/// container. `next_obj` and `next_state` therefore mirror qpdf's reactor
/// hand-off: `make_object` installs an empty canonical container, and
/// `container_start` puts that same handle on the stack before the child
/// members arrive. The importer owns this live-handle route.
pub(crate) struct JsonReactor<'a, P, S>
where
    P: Read + Seek + 'static,
    S: Read + Seek + 'static,
{
    pdf: &'a mut Pdf<P>,
    source: Rc<RefCell<S>>,
    input_name: String,
    must_be_complete: bool,
    fatal_error: Option<String>,
    errors: bool,
    saw_qpdf: bool,
    saw_qpdf_meta: bool,
    saw_objects: bool,
    saw_json_version: bool,
    saw_pdf_version: bool,
    saw_trailer: bool,
    cur_object: String,
    saw_value: bool,
    saw_stream: bool,
    saw_dict: bool,
    saw_data: bool,
    saw_datafile: bool,
    this_stream_needs_data: bool,
    reserved: BTreeSet<ObjectRef>,
    stack: Vec<StackFrame>,
    next_obj: Option<ObjectHandle>,
    next_state: ReactorState,
    stream_data_base_offset: u64,
}

impl<'a, P, S> JsonReactor<'a, P, S>
where
    P: Read + Seek + 'static,
    S: Read + Seek + 'static,
{
    pub(crate) fn new(
        pdf: &'a mut Pdf<P>,
        source: Rc<RefCell<S>>,
        input_name: impl Into<String>,
        must_be_complete: bool,
    ) -> Self {
        let reserved = pdf
            .resolver
            .all_object_handles()
            .into_iter()
            .filter(|handle| handle.is_reserved())
            .filter_map(|handle| handle.object_ref())
            .collect();
        Self {
            pdf,
            source,
            input_name: input_name.into(),
            must_be_complete,
            fatal_error: None,
            errors: false,
            saw_qpdf: false,
            saw_qpdf_meta: false,
            saw_objects: false,
            saw_json_version: false,
            saw_pdf_version: false,
            saw_trailer: false,
            cur_object: String::new(),
            saw_value: false,
            saw_stream: false,
            saw_dict: false,
            saw_data: false,
            saw_datafile: false,
            this_stream_needs_data: false,
            reserved,
            stack: Vec::new(),
            next_obj: None,
            next_state: ReactorState::Top,
            stream_data_base_offset: 0,
        }
    }

    /// The source's own stream position when parsing began, added to every
    /// offset the tokenizer records before it is used to seek back into the
    /// source for a deferred inline stream read. Only [`super::document`]'s
    /// public entry points need this: their `source` is caller-supplied and
    /// may not already be positioned at its own logical start, unlike every
    /// other construction path in this module (all of which parse a source
    /// they control from position `0`).
    pub(crate) fn with_stream_data_base_offset(mut self, offset: u64) -> Self {
        self.stream_data_base_offset = offset;
        self
    }

    pub(crate) fn any_errors(&self) -> bool {
        self.errors
    }

    pub(crate) fn fatal_error(&self) -> Option<&str> {
        self.fatal_error.as_deref()
    }

    fn fatal(&mut self, message: impl Into<String>) {
        if self.fatal_error.is_none() {
            self.fatal_error = Some(message.into());
        }
    }

    fn error(&mut self, offset: i64, message: impl Into<String>) {
        self.errors = true;
        let result = self.pdf.resolver.push_json_warning(
            &self.input_name,
            &self.cur_object,
            offset,
            message,
        );
        if let Err(error) = result {
            self.fatal(error.to_string());
        }
    }

    fn container_start(&mut self) {
        let object = self.next_obj.take();
        self.stack.push(StackFrame {
            state: self.next_state,
            object,
        });
    }

    fn set_next_state_if_dictionary(
        &mut self,
        key: &str,
        value: &Json,
        next: ReactorState,
    ) -> bool {
        if value.is_dictionary() {
            self.next_state = next;
            true
        } else {
            self.error(value.start(), format!("\"{key}\" must be a dictionary"));
            false
        }
    }

    fn current_object(&mut self, context: &str) -> Option<ObjectHandle> {
        let Some(object) = self.stack.last().and_then(|frame| frame.object.clone()) else {
            self.fatal(format!("current object uninitialized in {context}")); // cov:ignore: parser state always supplies an object for object-bearing states
            return None; // cov:ignore: parser state always supplies an object for object-bearing states
        };
        if let Err(error) = object.try_dereference() {
            self.fatal(error.to_string());
            return None;
        }
        Some(object)
    }

    fn mark_dirty(&mut self, handle: &ObjectHandle) {
        if let Err(error) = self.pdf.mark_object_handle_dirty(handle) {
            self.fatal(error.to_string()); // cov:ignore: all parser-created handles are owned by this Pdf
        }
    }

    fn set_object_description(&self, handle: &ObjectHandle, value: &Json) {
        handle.set_description_json(
            self.input_name.clone(),
            self.cur_object.clone(),
            value.start(),
        );
    }

    fn make_empty_stream(&self) -> ObjectHandle {
        let dictionary = self
            .pdf
            .resolver
            .direct_object_handle(ObjectValue::Dictionary(BTreeMap::new()));
        self.pdf.resolver.direct_object_handle(ObjectValue::Stream {
            stream_dict: dictionary,
            stream_data: None,
            stream_length: 0,
            stream_provider: None,
            filter_on_write: true,
        })
    }

    fn make_object(&mut self, value: &Json) -> ObjectHandle {
        let result = if value.is_dictionary() {
            let result = self
                .pdf
                .resolver
                .direct_object_handle(ObjectValue::Dictionary(BTreeMap::new()));
            self.next_obj = Some(result.clone());
            self.next_state = ReactorState::Object;
            result
        } else if value.is_array() {
            let result = self
                .pdf
                .resolver
                .direct_object_handle(ObjectValue::Array(Vec::new()));
            self.next_obj = Some(result.clone());
            self.next_state = ReactorState::Object;
            result
        } else if let Some(string) = value.get_string() {
            if !is_qpdf_json_string(&string) {
                self.error(value.start(), "unrecognized string value");
                self.pdf.resolver.direct_object_handle(ObjectValue::Null)
            } else {
                match json_value_to_handle(self.pdf, value) {
                    Ok(result) => result,
                    Err(error) => {
                        self.fatal(error.to_string());
                        self.pdf.resolver.direct_object_handle(ObjectValue::Null)
                    }
                }
            }
        } else {
            match json_value_to_handle(self.pdf, value) {
                Ok(result) => result,
                // Number, Bool, and Null are the only kinds left; Bool and
                // Null never fail, and a Number's only failure mode is
                // invalid UTF-8 bytes -- unreachable here because the real
                // JSON tokenizer only ever populates a `Number` token from
                // JSON's ASCII number grammar.
                // cov:ignore-start: unreachable -- see comment above
                Err(error) => {
                    self.fatal(error.to_string());
                    self.pdf.resolver.direct_object_handle(ObjectValue::Null)
                } // cov:ignore-end
            }
        };

        if result.description().is_empty() {
            self.set_object_description(&result, value);
        }
        result
    }

    fn replace_object(&mut self, replacement: ObjectHandle, value: &Json) {
        if replacement.is_indirect() {
            self.error(
                replacement.get_parsed_offset(),
                "the value of an object may not be an indirect object reference",
            );
            return;
        }
        let Some(target) = self.current_object("st_object_top") else {
            return; // cov:ignore: parser state always supplies the st_object_top frame
        };
        let Some(object_ref) = target.object_ref() else {
            self.fatal("current object has no indirect identity in st_object_top"); // cov:ignore: object-top handles come from an obj:N G R key
            return; // cov:ignore: object-top handles come from an obj:N G R key
        };
        match self.pdf.replace_object(object_ref, replacement) {
            Ok(target) => {
                self.next_obj = Some(target.clone());
                self.set_object_description(&target, value);
            }
            Err(error) => self.fatal(error.to_string()), // cov:ignore: replacement is a same-Pdf direct value from make_object
        }
    }

    fn normalize_dangling_reserved(&mut self) {
        let dangling: Vec<ObjectRef> = self
            .pdf
            .resolver
            .all_object_handles()
            .into_iter()
            .filter(|handle| handle.is_reserved())
            .filter_map(|handle| handle.object_ref())
            .filter(|object_ref| !self.reserved.contains(object_ref))
            .collect();
        for object_ref in dangling {
            if let Err(error) = self.pdf.replace_object(object_ref, ObjectHandle::null()) {
                self.fatal(error.to_string()); // cov:ignore: reserved handles and the null replacement share this resolver
                return; // cov:ignore: reserved handles and the null replacement share this resolver
            }
        }
    }

    fn finish_container(&mut self, from_state: ReactorState, value: &Json) {
        if self.stack.is_empty() {
            self.fatal("JSONReactor::containerEnd stack is empty"); // cov:ignore: Reactor invokes finish_container only after observing a stack frame
            return; // cov:ignore: Reactor invokes finish_container only after observing a stack frame
        }
        self.stack.pop();
        if self.stack.is_empty() {
            if !self.saw_qpdf {
                self.error(0, "\"qpdf\" object was not seen");
            } else {
                if !self.saw_json_version {
                    self.error(0, "\"qpdf[0].jsonversion\" was not seen");
                }
                if self.must_be_complete && !self.saw_pdf_version {
                    self.error(0, "\"qpdf[0].pdfversion\" was not seen");
                }
                if !self.saw_objects {
                    self.error(0, "\"qpdf[1]\" was not seen");
                } else if self.must_be_complete && !self.saw_trailer {
                    self.error(0, "\"qpdf[1].trailer\" was not seen");
                }
            }
        } else if from_state == ReactorState::Trailer && !self.saw_value {
            self.error(value.start(), "\"trailer\" is missing \"value\"");
        } else if from_state == ReactorState::ObjectTop {
            if self.saw_value == self.saw_stream {
                self.error(
                    value.start(),
                    "object must have exactly one of \"value\" or \"stream\"",
                );
            }
            if self.saw_stream {
                if !self.saw_dict {
                    self.error(value.start(), "\"stream\" is missing \"dict\"");
                }
                if self.saw_data == self.saw_datafile {
                    if self.this_stream_needs_data {
                        self.error(
                            value.start(),
                            "new \"stream\" must have exactly one of \"data\" or \"datafile\"",
                        );
                    } else if self.saw_datafile {
                        self.error(
                            value.start(),
                            "existing \"stream\" may at most one of \"data\" or \"datafile\"",
                        );
                    }
                }
            }
        } else if from_state == ReactorState::Qpdf {
            self.normalize_dangling_reserved();
        }

        if self
            .stack
            .last()
            .is_some_and(|frame| frame.state == ReactorState::Objects)
        {
            self.cur_object.clear();
            self.saw_dict = false;
            self.saw_data = false;
            self.saw_datafile = false;
            self.saw_value = false;
            self.saw_stream = false;
        }
    }

    fn dictionary_item_impl(&mut self, key: &[u8], value: &Json) {
        let state = self.stack.last().map(|frame| frame.state);
        let Some(state) = state else {
            self.fatal("stack is empty in dictionaryItem");
            return;
        };
        self.next_state = ReactorState::Ignore;
        match state {
            ReactorState::Ignore => {}
            ReactorState::Top => {
                if key == b"qpdf" {
                    self.saw_qpdf = true;
                    if value.is_array() {
                        self.next_state = ReactorState::Qpdf;
                    } else {
                        self.error(value.start(), "\"qpdf\" must be an array");
                    }
                }
            }
            ReactorState::QpdfMeta => match key {
                b"pdfversion" => {
                    self.saw_pdf_version = true;
                    let valid = value
                        .get_string()
                        .and_then(|version| validate_pdf_version(&version));
                    if let Some(version) = valid {
                        self.pdf.version = version;
                    } else {
                        self.error(value.start(), "invalid PDF version (must be \"x.y\")");
                    }
                }
                b"jsonversion" => {
                    self.saw_json_version = true;
                    let mut overflow = None;
                    let okay = value.get_number().is_some_and(|number| {
                        // The JSON tokenizer only ever populates a `Number`
                        // token from JSON's number grammar (ASCII digits,
                        // sign, `.`, `e`/`E`), so this can never fail when
                        // driven through real parsing.
                        let Ok(text) = std::str::from_utf8(&number) else {
                            return false; // cov:ignore: unreachable -- JSON number tokens are always ASCII
                        };
                        match qpdf_string_to_int_checked(text) {
                            QpdfIntParse::Value(2) => true,
                            QpdfIntParse::Overflow(message) => {
                                overflow = Some(message);
                                true
                            }
                            QpdfIntParse::Value(_) | QpdfIntParse::NoDigits => false,
                        }
                    });
                    if let Some(message) = overflow {
                        self.fatal(message);
                    } else if !okay {
                        self.error(
                            value.start(),
                            "invalid JSON version (must be numeric value 2)",
                        );
                    }
                }
                b"pushedinheritedpageresources" => match value.get_bool() {
                    Some(true) if !self.must_be_complete => {
                        if let Err(error) = crate::PageDocumentHelper::new(self.pdf)
                            .push_inherited_attributes_to_pages()
                        {
                            self.fatal(error.to_string());
                        }
                    }
                    Some(_) => {}
                    None => self.error(
                        value.start(),
                        "pushedinheritedpageresources must be a boolean",
                    ),
                },
                b"calledgetallpages" => match value.get_bool() {
                    Some(true) if !self.must_be_complete => {
                        if let Err(error) = crate::PageDocumentHelper::new(self.pdf).get_all_pages()
                        {
                            self.fatal(error.to_string());
                        }
                    }
                    Some(_) => {}
                    None => self.error(value.start(), "calledgetallpages must be a boolean"),
                },
                _ => {}
            },
            ReactorState::Objects => {
                if key == b"trailer" {
                    self.saw_trailer = true;
                    self.cur_object = "trailer".to_owned();
                    self.set_next_state_if_dictionary("trailer", value, ReactorState::Trailer);
                } else {
                    match parse_object_key(key) {
                        IndirectReferenceParse::Reference(object_ref) => {
                            self.cur_object = String::from_utf8_lossy(key).into_owned();
                            if self.set_next_state_if_dictionary(
                                &self.cur_object.clone(),
                                value,
                                ReactorState::ObjectTop,
                            ) {
                                self.next_obj = Some(
                                    self.pdf.resolver.reserve_object_if_not_exists(object_ref),
                                );
                            }
                        }
                        IndirectReferenceParse::Overflow(message) => self.fatal(message),
                        IndirectReferenceParse::NoMatch => {
                            self.error(
                                value.start(),
                                "object key should be \"trailer\" or \"obj:n n R\"",
                            );
                        }
                    }
                }
            }
            ReactorState::ObjectTop => {
                let Some(current) = self.current_object("st_object_top") else {
                    return;
                };
                if key == b"value" {
                    self.saw_value = true;
                    let replacement = self.make_object(value);
                    self.replace_object(replacement, value);
                    self.next_state = ReactorState::Object;
                } else if key == b"stream" {
                    self.saw_stream = true;
                    if self.set_next_state_if_dictionary("stream", value, ReactorState::Stream) {
                        self.this_stream_needs_data = false;
                        let is_stream = current.as_stream_dict().is_some();
                        if !is_stream {
                            self.this_stream_needs_data = true;
                            let replacement = self.make_empty_stream();
                            let Some(object_ref) = current.object_ref() else {
                                self.fatal("current object has no indirect identity in stream"); // cov:ignore: stream objects originate from an indirect obj:N G R key
                                return; // cov:ignore: stream objects originate from an indirect obj:N G R key
                            };
                            match self.pdf.replace_object(object_ref, replacement) {
                                Ok(target) => {
                                    self.set_object_description(&target, value);
                                    self.next_obj = Some(target);
                                }
                                Err(error) => self.fatal(error.to_string()), // cov:ignore: replacement is a same-Pdf canonical stream value
                            }
                        } else {
                            self.next_obj = Some(current);
                        }
                    }
                }
            }
            ReactorState::Trailer => {
                if key == b"value" {
                    self.saw_value = true;
                    if self.set_next_state_if_dictionary(
                        "trailer.value",
                        value,
                        ReactorState::Object,
                    ) {
                        let trailer = self.make_object(value);
                        self.pdf.trailer_handle_memo = Some(trailer.clone());
                        self.set_object_description(&trailer, value);
                    }
                } else if key == b"stream" {
                    self.error(value.start(), "the trailer may not be a stream");
                }
            }
            ReactorState::Stream => {
                let Some(current) = self.current_object("st_stream") else {
                    return; // cov:ignore: the stream state is entered only with next_obj populated
                };
                if current.as_stream_dict().is_none() {
                    self.fatal("current object is not stream in st_stream"); // cov:ignore: st_stream is installed only after reserveStream-equivalent replacement
                    return; // cov:ignore: st_stream is installed only after reserveStream-equivalent replacement
                }
                match key {
                    b"dict" => {
                        self.saw_dict = true;
                        if self.set_next_state_if_dictionary(
                            "stream.dict",
                            value,
                            ReactorState::Object,
                        ) {
                            let dictionary = self.make_object(value);
                            // cov:ignore-start: make_object and replace_stream_dict share this Pdf-owned dictionary
                            if let Err(error) = current.replace_stream_dict(dictionary) {
                                self.fatal(error.to_string());
                            } else {
                                self.mark_dirty(&current);
                            }
                            // cov:ignore-end
                        }
                    }
                    b"data" => {
                        self.saw_data = true;
                        if value.get_string().is_none() {
                            self.error(value.start(), "\"stream.data\" must be a string");
                            current.replace_stream_data(Rc::new(Vec::new()), None, None);
                            self.mark_dirty(&current);
                        } else {
                            match inline_stream_data_provider(
                                self.source.clone(),
                                value,
                                self.stream_data_base_offset,
                            ) {
                                Ok(provider) => {
                                    // cov:ignore-start: parser-created stream handles are always indirect and same-Pdf
                                    if let Err(error) =
                                        current.replace_stream_data_provider(provider, None, None)
                                    {
                                        self.fatal(error.to_string());
                                    } else {
                                        self.mark_dirty(&current);
                                    }
                                    // cov:ignore-end
                                }
                                Err(error) => self.fatal(error.to_string()), // cov:ignore: parser JSON string ranges are validated by Json
                            }
                        }
                    }
                    b"datafile" => {
                        self.saw_datafile = true;
                        let Some(filename) = value.get_string() else {
                            self.error(
                                value.start(),
                                "\"stream.datafile\" must be a string containing a file name",
                            );
                            current.replace_stream_data(Rc::new(Vec::new()), None, None);
                            self.mark_dirty(&current);
                            return;
                        };
                        let provider = datafile_stream_data_provider(PathBuf::from(
                            String::from_utf8_lossy(&filename).into_owned(),
                        ));
                        // cov:ignore-start: parser-created stream handles are always indirect and same-Pdf
                        if let Err(error) =
                            current.replace_stream_data_provider(provider, None, None)
                        {
                            self.fatal(error.to_string());
                        } else {
                            self.mark_dirty(&current);
                        }
                        // cov:ignore-end
                    }
                    _ => {}
                }
            }
            ReactorState::Object => {
                let Some(current) = self.current_object("st_object") else {
                    return; // cov:ignore: object state is entered only with next_obj populated
                };
                let dictionary = current.as_stream_dict().unwrap_or_else(|| current.clone());
                if dictionary.as_dictionary().is_none() {
                    // cov:ignore-start: qpdf parser state guarantees dictionary/array container shape here
                    if let Err(error) =
                        dictionary.type_warning("dictionary", "ignoring key replacement request")
                    {
                        self.fatal(error.to_string());
                    }
                    return;
                    // cov:ignore-end
                }
                let key = match json_dictionary_key(key) {
                    Ok(key) => key,
                    Err(error) => {
                        self.fatal(error.to_string());
                        return;
                    }
                };
                let value = self.make_object(value);
                if let Err(error) = dictionary.replace_key(&key, value) {
                    self.fatal(error.to_string()); // cov:ignore: key values are made by this Pdf and ownership is checked at construction
                } else {
                    self.mark_dirty(&dictionary);
                }
            }
            ReactorState::Qpdf => {} // cov:ignore: JSON array callbacks select qpdf metadata or objects before dictionary events
        }
    }

    fn array_item_impl(&mut self, value: &Json) {
        let state = self.stack.last().map(|frame| frame.state);
        let Some(state) = state else {
            self.fatal("stack is empty in arrayItem");
            return;
        };
        self.next_state = ReactorState::Ignore;
        match state {
            ReactorState::Qpdf => {
                if !self.saw_qpdf_meta {
                    self.saw_qpdf_meta = true;
                    self.set_next_state_if_dictionary("qpdf[0]", value, ReactorState::QpdfMeta);
                } else if !self.saw_objects {
                    self.saw_objects = true;
                    self.set_next_state_if_dictionary("qpdf[1]", value, ReactorState::Objects);
                } else {
                    self.error(value.start(), "\"qpdf\" must have two elements");
                }
            }
            ReactorState::Object => {
                let Some(current) = self.current_object("st_object array item") else {
                    return; // cov:ignore: object state is entered only with next_obj populated
                };
                let item = self.make_object(value);
                if let Err(error) = current.append_array_item(item) {
                    self.fatal(error.to_string()); // cov:ignore: make_object installs an array before array callbacks
                } else {
                    self.mark_dirty(&current);
                }
            }
            _ => {}
        }
    }
}

impl<'a, P, S> Reactor for JsonReactor<'a, P, S>
where
    P: Read + Seek + 'static,
    S: Read + Seek + 'static,
{
    fn dictionary_start(&mut self) {
        if self.fatal_error.is_none() {
            self.container_start();
        }
    }

    fn array_start(&mut self) {
        if self.fatal_error.is_some() {
            return;
        }
        if self.stack.is_empty() {
            self.fatal("QPDF JSON must be a dictionary");
        } else {
            self.container_start();
        }
    }

    fn container_end(&mut self, value: &Json) {
        if self.fatal_error.is_none() {
            let state = self.stack.last().map(|frame| frame.state);
            if let Some(state) = state {
                self.finish_container(state, value);
            } else {
                self.fatal("JSONReactor::containerEnd stack is empty");
            }
        }
    }

    fn top_level_scalar(&mut self) {
        if self.fatal_error.is_none() {
            self.fatal("QPDF JSON must be a dictionary");
        }
    }

    fn dictionary_item(&mut self, key: &[u8], value: &Json) -> bool {
        if self.fatal_error.is_none() {
            self.dictionary_item_impl(key, value);
        }
        true
    }

    fn array_item(&mut self, value: &Json) -> bool {
        if self.fatal_error.is_none() {
            self.array_item_impl(value);
        }
        true
    }
}

fn is_qpdf_json_string(value: &[u8]) -> bool {
    if !matches!(
        parse_indirect_reference(value),
        IndirectReferenceParse::NoMatch
    ) || value.starts_with(b"u:")
        || value.starts_with(b"n:/") && value.len() > 3
    {
        return true;
    }
    if let Some(binary) = value.strip_prefix(b"b:") {
        return binary.len() % 2 == 0 && binary.iter().all(u8::is_ascii_hexdigit);
    }
    value.len() > 1 && value[0] == b'/'
}

fn validate_pdf_version(value: &[u8]) -> Option<String> {
    let dot = value.iter().position(|byte| *byte == b'.')?;
    if dot == 0
        || dot + 1 == value.len()
        || !value[..dot].iter().all(u8::is_ascii_digit)
        || !value[dot + 1..].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    String::from_utf8(value.to_vec()).ok()
}
