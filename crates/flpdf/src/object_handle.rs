//! The core object-handle graph: shared, cloneable identity for direct and
//! indirect PDF objects, with qpdf-compatible parsed-offset tracking and the
//! document-owned reserved construction sentinel.
//!
//! qpdf correspondence: `QPDFObjectHandle`, `QPDFObject`, and `QPDFValue` identity and payload ownership, `QPDF::newReserved`/`QPDF_Reserved`, `QPDFObjectHandle::copyStream`/`QPDF::copyStreamData` stream-copy primitives, `QPDF::setImmediateCopyFrom`, plus `QPDFWriter.cc` `unparseObject`/`writeTrailer` writer-emission primitives (`unparse_object`/`unparse_object_qdf`/`unparse_stream_body`/`unparse_stream_body_qdf`/`unparse_trailer`).
//!
//! `QPDFObjectHandle` holds `std::shared_ptr<QPDFObject>` and defines object
//! sameness by that pointer, not by structural equality
//! (`include/qpdf/QPDFObjectHandle.hh:304-309,1338-1350`,
//! `libqpdf/QPDFObjectHandle.cc:224-227`). [`ObjectHandle`] maps that one
//! shared allocation to one uniform `Rc<RefCell<ObjectSlot>>`: direct and
//! indirect forms therefore share one identity, payload, and metadata slot.
//!
//! qpdf's `QPDFObject` owns one shared `QPDFValue`; that value carries the
//! current `QPDF*`, object generation, and parsed offset
//! (`libqpdf/qpdf/QPDFObject_private.hh:19-29,60-68,117-150,176-180`,
//! `libqpdf/qpdf/QPDFValue.hh:60-72,90-110,144-152`). Resolution reads the
//! current metadata from this same allocation
//! (`libqpdf/QPDFObject.cc:7-16`). `ObjectSlot` likewise keeps the active
//! object reference, resolver, parsed offset, and value state together.
//!
//! qpdf promotion registers and updates the existing allocation rather than
//! cloning a direct payload (`libqpdf/QPDF.cc:1835-1839,1882-1897`); this
//! port's [`ObjectHandle::promote_to_indirect`] mutates its existing slot on
//! the same boundary. During teardown qpdf disconnects every cached indirect
//! object, including unresolved entries, and destroys only non-null values
//! (`libqpdf/QPDF.cc:215-235`); this port's
//! [`ObjectHandle::disconnect`] clears the same slot's indirect metadata and
//! state-sensitive value.
//!
//! `QPDFObjectHandle.cc:456-466,759-785,869-955,1027-1039` supplies the
//! name/dictionary/array inspection and live array mutation mirrored by
//! `try_is_name_and_equals`, `try_is_dictionary_of_type`, `try_array_len`,
//! `try_array_item`, `set_array_item`, `set_array_items`,
//! `insert_array_item`, `append_array_item`, and `erase_array_item`.
//!
//! `QPDFObjectHandle` (`include/qpdf/QPDFObjectHandle.hh`) shares a canonical `QPDFObject`
//! (`libqpdf/qpdf/QPDFObject.hh`), which owns the `QPDFValue` payload
//! (`libqpdf/qpdf/QPDFValue.hh`).

// Deviation: shared handle identity uses Rc<RefCell<..>> in place of qpdf's
// std::shared_ptr<QPDFObject>; ObjectValue is the QPDFValue payload. This is
// internal structure only and does not affect output bytes (see
// docs/qpdf-correspondence.md).
//
// Deviation: qpdf's canonical name strings include a leading slash and its
// array access borrows QPDF_Array, while ObjectValue stores decoded name bytes
// without the slash and Vec<ObjectHandle>. Dictionary keys, however, retain
// qpdf's canonical leading slash in the ObjectHandle graph. Inspection
// compares the same decoded name bytes and clones only one Rc-backed child per
// valid array access. It emits no bytes or diagnostics; invalid array access
// (where qpdf warns) is outside the try_array_item contract. See
// docs/qpdf-correspondence.md.

use crate::{
    content_normalizer::ContentNormalizerPipeline,
    pipeline::{
        count::Count,
        flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE},
        Pipeline, PipelineError, PipelineRef,
    },
    stream_filter::{
        decode_params_from_handle, normalize_filter_name, stream_filter_for, OwnedDecodePipeline,
        StreamFilter, DECODE_PARMS_LENGTH_ERROR, FILTER_TYPE_ERROR,
    },
    writer::DecodeLevel,
};
use crate::{Dictionary, Error, Object, ObjectRef, Result, Stream};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

/// qpdf's `qpdf_ef_compress` bit in `QPDF_Stream::pipeStreamData`.
pub(crate) const STREAM_ENCODE_COMPRESS: u32 = 1;

/// qpdf's `qpdf_ef_normalize` bit in `QPDF_Stream::pipeStreamData`.
pub(crate) const STREAM_ENCODE_NORMALIZE: u32 = 2;

const STREAM_DATA_PROVIDER_DEFAULT_ERROR: &str =
    "you must override provideStreamData -- see QPDFObjectHandle.hh";

/// Deferred stream-data source corresponding to qpdf's
/// `QPDFObjectHandle::StreamDataProvider`
/// (`include/qpdf/QPDFObjectHandle.hh:68-127`).
///
/// Registration retains this trait object and does not invoke it. The source
/// is called only when the stream is piped, and every invocation for one
/// stream must produce the same bytes. Providers must not mutate PDF objects
/// while producing data because qpdf may invoke them more than once during a
/// linearized write.
///
/// Rust uses distinct method names for qpdf's overloaded forms. The
/// `ObjectRef` methods delegate to the numeric identity methods by default,
/// matching qpdf's `QPDFObjGen` overload delegation. A provider that supports
/// the retry-aware form must return `true` from [`Self::supports_retry`].
pub trait StreamDataProvider {
    /// Whether the retry-aware success-returning callback should be used.
    fn supports_retry(&self) -> bool {
        false
    }

    /// Legacy provider form receiving the complete stream identity.
    fn provide_stream_data(
        &self,
        object_ref: ObjectRef,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        self.provide_stream_data_by_id(object_ref.number, object_ref.generation, pipeline)
    }

    /// Legacy provider form receiving numeric object identity.
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        _pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        Err(Error::Internal(
            STREAM_DATA_PROVIDER_DEFAULT_ERROR.to_owned(),
        ))
    }

    /// Retry-aware provider form receiving the complete stream identity.
    fn provide_stream_data_with_retry(
        &self,
        object_ref: ObjectRef,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        self.provide_stream_data_with_retry_by_id(
            object_ref.number,
            object_ref.generation,
            pipeline,
            suppress_warnings,
            will_retry,
        )
    }

    /// Retry-aware provider form receiving numeric object identity.
    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        _pipeline: &mut dyn Pipeline,
        _suppress_warnings: bool,
        _will_retry: bool,
    ) -> Result<bool> {
        Err(Error::Internal(
            STREAM_DATA_PROVIDER_DEFAULT_ERROR.to_owned(),
        ))
    }
}

impl std::fmt::Debug for dyn StreamDataProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StreamDataProvider(..)")
    }
}

struct CallbackProvider<F> {
    callback: F,
}

impl<F> StreamDataProvider for CallbackProvider<F>
where
    F: Fn(&mut dyn Pipeline) -> Result<()> + 'static,
{
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        (self.callback)(pipeline)
    }
}

struct RetryCallbackProvider<F> {
    callback: F,
}

impl<F> StreamDataProvider for RetryCallbackProvider<F>
where
    F: Fn(&mut dyn Pipeline, bool, bool) -> Result<bool> + 'static,
{
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        (self.callback)(pipeline, suppress_warnings, will_retry)
    }
}

/// qpdf's `CopiedStreamDataProvider` (`libqpdf/QPDF.cc:126-163`), retaining
/// the source handle so copied provider/original streams are still dispatched
/// through the source document when the destination stream is read.
struct CopiedStreamDataProvider {
    source: ObjectHandle,
}

impl StreamDataProvider for CopiedStreamDataProvider {
    fn supports_retry(&self) -> bool {
        true
    }

    fn provide_stream_data_with_retry_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        let mut filtering_attempted = false;
        self.source.pipe_stream_data(
            pipeline,
            &mut filtering_attempted,
            0,
            DecodeLevel::None,
            suppress_warnings,
            will_retry,
        )
    }
}

pub(crate) fn copied_stream_data_provider(source: ObjectHandle) -> Rc<dyn StreamDataProvider> {
    Rc::new(CopiedStreamDataProvider { source })
}

struct StreamFilterPlan {
    filters: Vec<Box<dyn StreamFilter>>,
    specialized_compression: bool,
    lossy_compression: bool,
}

/// The no-offset sentinel qpdf uses for values that were not parsed from a
/// source position (`QPDFValue`'s parsed offset starts at `-1` and is set
/// only while still negative; see
/// `libqpdf/qpdf/QPDFValue.hh:90-100,149-152`).
pub(crate) const NO_PARSED_OFFSET: i64 = -1;

/// The conflicts-tracking map [`ObjectHandle::merge_resources`] populates:
/// `rtype -> old_key -> new_key`, mirroring
/// `QPDFObjectHandle::mergeResources`'s own
/// `std::map<std::string, std::map<std::string, std::string>>` parameter.
pub type ResourceConflicts =
    std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>>;

/// The document-owned resolver qpdf's `QPDFObject` calls through its owning
/// `QPDF*` and object identity. Kept crate-private so only the canonical
/// document implementation can resolve an indirect slot.
pub(crate) trait DocumentResolver {
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()>;

    /// Create qpdf's owned empty stream object for an ObjectHandle operation.
    fn new_stream(&self) -> Result<ObjectHandle> {
        Err(Error::Internal(
            "stream creation requested from a resolver without a document".to_owned(),
        ))
    }

    /// Copy raw stream data into an already-created destination stream. The
    /// destination resolver owns the provider registration, while the source
    /// handle retains the source document's lazy dispatch boundary.
    fn copy_stream_data(&self, destination: &ObjectHandle, source: &ObjectHandle) -> Result<()> {
        let _ = (destination, source);
        Err(Error::Internal(
            "stream data copy requested from a resolver without a document".to_owned(),
        ))
    }

    /// Whether this resolver is a qpdf source configured for immediate stream
    /// copying (`QPDF::setImmediateCopyFrom`).
    fn immediate_copy_from(&self) -> bool {
        false
    }

    /// The document-side half of `QPDFObjectHandle::warn`
    /// (`libqpdf/QPDFObjectHandle.cc:2385-2396`): `QPDF::warn`
    /// (`libqpdf/QPDF.cc:487-494`) reached from an object rather than from a
    /// caller that holds the document.
    ///
    /// The message arrives fully formed, as qpdf's `QPDFExc` is by the time
    /// it reaches `QPDF::warn`. The exception filename is `""`; its object
    /// slot is filled from the handle description before the message reaches
    /// this sink. Live parser values now carry that description, while
    /// programmatic handles retain their existing empty/object-reference
    /// fallback.
    ///
    /// The default reports rather than swallows, matching
    /// [`Self::pipe_stream_data`]'s. Every document-backed resolver overrides
    /// it; qpdf has no resolver that cannot warn, so reaching this default is
    /// the same condition as qpdf's null context, which `QPDFObjectHandle::warn`
    /// also turns into a thrown exception.
    #[allow(dead_code)] // reached once the try_* accessors gain production
                        // consumers in flpdf-25kg.3.6
    fn warn(&self, message: String) -> Result<()> {
        Err(Error::Internal(format!(
            "warning raised through a resolver with no document warning sink: {message}"
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn pipe_stream_data(
        &self,
        object_ref: ObjectRef,
        offset: i64,
        length: usize,
        stream_dict: &ObjectHandle,
        pipeline: &mut dyn crate::pipeline::Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        let _ = (
            object_ref,
            offset,
            length,
            stream_dict,
            pipeline,
            suppress_warnings,
            will_retry,
        );
        Err(Error::Internal(
            "stream data requested from a resolver without a stream source".to_owned(),
        ))
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parse_without_context_rejects_a_nested_indirect_reference() {
        // qpdf's `QPDFObjectHandle::parse("[1 0 R]")` has no owning
        // document and therefore throws `std::logic_error` rather than
        // inventing a detached reference. Returning a successfully parsed
        // array here would hide the missing context boundary.
        let error = ObjectHandle::parse(b"[1 0 R]").expect_err("missing parse context");

        assert_eq!(
            error.to_string(),
            "QPDFParser::parse called without context on an object with indirect references"
        );
    }

    #[test]
    fn parse_without_context_keeps_nested_direct_values_contextless() {
        let parsed = ObjectHandle::parse(b"<< /L1 << /L2 7 >> >>")
            .expect("direct values do not need a parse context");
        let level_one = parsed
            .as_dictionary()
            .and_then(|values| values.get(b"/L1".as_slice()).cloned())
            .expect("level one dictionary");
        let level_two = level_one
            .as_dictionary()
            .and_then(|values| values.get(b"/L2".as_slice()).cloned())
            .expect("level two scalar");

        let error = level_two
            .object_warning("contextless explicit parse")
            .expect_err("an explicit parse must not acquire a document context");

        assert!(matches!(
            error,
            crate::Error::System(message)
                if message == "parsed object,  at offset 14: contextless explicit parse"
        ));
    }

    #[test]
    fn parse_without_context_stamps_the_qpdf_parsed_object_description() {
        let parsed = ObjectHandle::parse(b"<< /Value 7 >>")
            .expect("direct values do not need a parse context");

        assert_eq!(parsed.description(), "parsed object,  at offset 2");
        let value = parsed
            .as_dictionary()
            .and_then(|values| values.get(b"/Value".as_slice()).cloned())
            .expect("parsed scalar");
        assert_eq!(value.description(), "parsed object,  at offset 10");

        let error = value
            .object_warning("contextless explicit parse")
            .expect_err("an explicit parse must remain contextless");
        assert!(matches!(
            error,
            crate::Error::System(message)
                if message == "parsed object,  at offset 10: contextless explicit parse"
        ));
    }

    #[test]
    fn parse_without_context_turns_recovery_warnings_into_errors() {
        let error = ObjectHandle::parse(b"{").expect_err("qpdf warning must fail explicit parse");

        assert!(matches!(
            error,
            crate::Error::Parse {
                offset: 0,
                ref message,
            } if message == "treating unexpected brace token as null"
        ));
    }

    #[test]
    fn parse_without_context_rejects_the_501st_nested_container() {
        // qpdf's heap-owned parser stack accepts 500 containers, then warns
        // for the 501st. With no QPDF context, `QPDFParser::warn` throws that
        // warning instead of recording it on the document.
        let input = vec![b'['; 501];
        let error =
            ObjectHandle::parse(&input).expect_err("depth warning must fail explicit parse");

        assert!(matches!(
            error,
            crate::Error::Parse { ref message, .. }
                if message == "ignoring excessively deeply nested data structure"
        ));
    }

    #[test]
    fn parse_without_context_keeps_the_first_warning_ahead_of_later_references() {
        // `QPDFParser::warn` throws immediately when `context == nullptr`.
        // The later `1 0 R` must not replace the earlier brace warning with
        // the no-context indirect-reference logic error.
        let error = ObjectHandle::parse(b"[ { 1 0 R ]")
            .expect_err("the first recoverable condition must terminate parse");

        assert!(matches!(
            error,
            crate::Error::Parse {
                offset: 2,
                ref message,
            } if message == "treating unexpected brace token as null"
        ));
    }

    #[test]
    fn parse_without_context_propagates_each_late_recovery_warning() {
        for (input, expected) in [
            (
                b"bare-word".as_slice(),
                "unknown token while reading object; treating as string",
            ),
            (
                b"<< /Last >>".as_slice(),
                "dictionary ended prematurely; using null as value for last key",
            ),
            (
                b"<< /QPDFFake1 1 2 >>".as_slice(),
                "expected dictionary key but found non-name object; inserting key /QPDFFake2",
            ),
            (
                b"<< /K 1 /K 2 >>".as_slice(),
                "dictionary has duplicated key /K; last occurrence overrides earlier ones",
            ),
        ] {
            let error = ObjectHandle::parse(input).expect_err("qpdf warning must fail parse");
            assert!(matches!(
                error,
                crate::Error::Parse { ref message, .. } if message == expected
            ));
        }
    }

    #[test]
    fn parse_without_context_allows_only_c_whitespace_after_the_object() {
        let parsed = ObjectHandle::parse(b"1 \t\r\n").expect("C whitespace is allowed");
        assert_eq!(parsed.as_integer(), Some(1));

        let error = ObjectHandle::parse(b"1 % not trailing whitespace")
            .expect_err("a PDF comment is not C whitespace");
        assert!(matches!(
            error,
            crate::Error::Parse {
                offset: 1,
                ref message,
            } if message == "trailing data found parsing object from string"
        ));
    }
}

/// A shared, cloneable handle to a PDF object.
///
/// Cloning a handle is O(1) and does not deep-copy the underlying value;
/// every clone, direct or indirect, shares one canonical identity, payload,
/// and resolution state. This maps qpdf's shared `QPDFObject` allocation and
/// pointer-identity `isSameObjectAs` comparison
/// (`include/qpdf/QPDFObjectHandle.hh:304-309,1338-1350`,
/// `libqpdf/QPDFObjectHandle.cc:224-227`).
///
/// Lazy dereference stays crate-internal until every document-created handle
/// is attached to the complete qpdf-native resolver.
///
/// ```compile_fail
/// let handle = flpdf::ObjectHandle::integer(1);
/// handle.try_dereference()?;
/// # Ok::<(), flpdf::Error>(())
/// ```
#[derive(Clone)]
pub struct ObjectHandle(Rc<RefCell<ObjectSlot>>);

// Hand-written rather than derived: a resolved value can hold other
// `ObjectHandle`s sharing canonical `Rc` identities (array/dict/stream-dict
// children — see `Pdf::drop`'s own comment on indirect cycles). A self- or
// reciprocal reference would make a derived, recursively-expanding `Debug`
// walk back into the same slot forever. Snapshot only metadata and the state
// name, never the resolved value, so formatting is total for direct and
// indirect cycles alike.
impl std::fmt::Debug for ObjectHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let slot = self.0.borrow();
        let state = slot.state.borrow();
        let state: &str = match &*state {
            ObjectState::NotYetResolved => "NotYetResolved",
            ObjectState::Resolved(_) => "Resolved(..)",
            ObjectState::Missing => "Missing",
            ObjectState::Reserved => "Reserved",
            ObjectState::Destroyed => "Destroyed",
        };
        let label = if slot.object_ref.is_some() {
            "ObjectHandle::Indirect"
        } else {
            "ObjectHandle::Direct"
        };
        f.debug_struct(label)
            .field("object_ref", &slot.object_ref)
            .field("state", &state)
            .field("parsed_offset", &slot.parsed_offset)
            .field("end_before_space", &slot.end_before_space)
            .field("end_after_space", &slot.end_after_space)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct ChildDescription {
    parent: Weak<RefCell<ObjectSlot>>,
    static_descr: String,
    var_descr: String,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct JsonDescription {
    pub(crate) input: String,
    pub(crate) object: String,
}

#[derive(Clone)]
pub(crate) enum ObjectDescription {
    Template(String),
    #[allow(dead_code)]
    Json(JsonDescription),
    Child(ChildDescription),
}

fn expand_description_template(
    template: &str,
    object_ref: Option<ObjectRef>,
    state: &ObjectState,
    parsed_offset: i64,
) -> String {
    let og = object_ref
        .map(|object_ref| format!("{} {}", object_ref.number, object_ref.generation))
        .unwrap_or_default();
    let shift = match state {
        ObjectState::Resolved(value) => match value {
            ObjectValue::Dictionary(_) | ObjectValue::Stream { .. } => 2,
            ObjectValue::Array(_) => 1,
            _ => 0,
        },
        _ => 0,
    };
    let offset = if parsed_offset >= 0 {
        (parsed_offset + shift).to_string()
    } else {
        parsed_offset.to_string()
    };

    // qpdf's QPDFValue::getDescription performs one `find`/`replace` for
    // each marker (`libqpdf/QPDFValue.cc:23-31`). There is no `$$` escape
    // convention: unknown and repeated markers remain in the result.
    let mut result = template.to_owned();
    if let Some(position) = result.find("$OG") {
        result.replace_range(position..position + 3, &og);
    }
    if let Some(position) = result.find("$PO") {
        result.replace_range(position..position + 3, &offset);
    }
    result
}

// Deliberately not `Debug`: see `ObjectHandle`'s own hand-written `Debug`
// impl above for why a derived one is unsafe here (object-handle cycles).
// This uniform allocation corresponds to qpdf's QPDFObject/QPDFValue pair:
// it keeps the current payload and all indirect metadata together rather
// than placing direct and indirect forms in separate backing storage.
struct ObjectSlot {
    /// The payload state is separately reference-counted so qpdf's
    /// `QPDFObject::assign` boundary can make two distinct handles observe
    /// one replacement value while retaining their own handle identities.
    state: Rc<RefCell<ObjectState>>,
    /// Every slot whose payload is the [`Self::state`] allocation. Normally
    /// this contains only the slot itself; qpdf's `QPDFObject::assign` can
    /// temporarily make a direct replacement handle and an indirect target
    /// share one payload while retaining distinct handle identities.
    ///
    /// The weak back-links let a later mutation through either alias update
    /// containment edges for every owner of the shared payload. Without this
    /// list, mutating the direct replacement after `replaceObject` would
    /// update only its own parent edges while the canonical target retained
    /// stale ownership metadata.
    state_owners: Rc<RefCell<Vec<Weak<RefCell<ObjectSlot>>>>>,
    object_ref: Option<ObjectRef>,
    active_pdf_unique_id: Option<u64>,
    resolver: Option<Weak<dyn DocumentResolver>>,
    parsed_offset: i64,
    end_before_space: i64,
    end_after_space: i64,
    pdf_unique_ids: BTreeSet<u64>,
    containment_parents: Vec<Weak<RefCell<ObjectSlot>>>,
    description: Option<ObjectDescription>,
}

impl ObjectSlot {
    fn get_description(&self) -> String {
        let state = self.state.borrow();
        if let Some(desc) = &self.description {
            match desc {
                ObjectDescription::Template(tmpl) => {
                    expand_description_template(tmpl, self.object_ref, &state, self.parsed_offset)
                }
                ObjectDescription::Json(j) => {
                    let obj_part = if j.object.is_empty() {
                        String::new()
                    } else {
                        format!(", {}", j.object)
                    };
                    format!("{}{obj_part} at offset {}", j.input, self.parsed_offset)
                }
                ObjectDescription::Child(child) => {
                    let mut result = String::new();
                    if let Some(parent_slot) = child.parent.upgrade() {
                        result = parent_slot.borrow().get_description();
                    }
                    result.push_str(&child.static_descr);
                    // qpdf's child branch replaces only the first marker in
                    // the already-rendered parent/static string
                    // (`libqpdf/QPDFValue.cc:52-54`).
                    if let Some(position) = result.find("$VD") {
                        result.replace_range(position..position + 3, &child.var_descr);
                    }
                    result
                }
            }
        } else if let Some(object_ref) = self.object_ref {
            format!("object {} {}", object_ref.number, object_ref.generation)
        } else {
            String::new()
        }
    }
}

/// The value payload of a direct `ObjectHandle`, mirroring qpdf's
/// `QPDFValue` type family (`libqpdf/qpdf/QPDFValue.hh`) and this crate's
/// existing [`crate::Object`] enum.
///
/// Array and dictionary children are [`ObjectHandle`]s rather than raw
/// nested `ObjectValue`s, so cloning a container clones only `Rc` handles
/// (O(1) per child), not the subtree.
#[derive(Debug, Clone)]
pub(crate) enum ObjectValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    /// Preserves a non-canonical source spelling (e.g. `.4`) alongside its
    /// parsed value, mirroring [`crate::Object::RealLiteral`], so that a
    /// real number written in the source PDF unparses byte-identically.
    RealLiteral {
        value: f64,
        literal: Vec<u8>,
    },
    Name(Vec<u8>),
    String(Vec<u8>),
    /// A content-stream operator token (e.g. `q`, `Do`), mirroring
    /// [`crate::Object::Operator`]. Only meaningful inside a content stream
    /// (`include/qpdf/QPDFObjectHandle.hh:318-319`: "Operator and
    /// InlineImage are only allowed in content streams").
    Operator(Vec<u8>),
    /// Raw inline-image (`BI`...`ID`...`EI`) bytes, mirroring
    /// [`crate::Object::InlineImage`]. Same content-stream-only constraint
    /// as `Operator` above.
    InlineImage(Vec<u8>),
    Array(Vec<ObjectHandle>),
    Dictionary(std::collections::BTreeMap<Vec<u8>, ObjectHandle>),
    /// A stream's own value: its dictionary (a separately parsed handle
    /// carrying its own `<<`-start parsed offset) and its optional replacement
    /// buffer. The stream value's own parsed offset (see
    /// [`ObjectHandle::get_parsed_offset`]) is the encoded stream-data
    /// start, distinct from the dictionary's.
    ///
    /// `stream_data: None` is qpdf's original-source state: no payload is
    /// retained at parse time, and `QPDF_Stream::pipeStreamData`
    /// (`libqpdf/QPDF_Stream.cc:571-620`) reads the owning document at this
    /// value's parsed offset instead. `Some` is the shared replacement buffer,
    /// mirroring qpdf's `std::shared_ptr<Buffer> stream_data`
    /// (`libqpdf/qpdf/QPDF_Stream.hh:104`).
    /// The sharing is observable behaviour, not a micro-optimization:
    /// `QPDF::copyStreamData` (`libqpdf/QPDF.cc:2240,2256-2258`) hands one
    /// stream's buffer to a second stream — in a different document — with no
    /// byte copy, and qpdf refuses to duplicate a stream payload at all
    /// (`QPDF_Stream::copy` throws, `libqpdf/QPDF_Stream.cc:141-144`).
    ///
    /// Sharing is sound here because the payload is never written through:
    /// [`ObjectHandle::replace_stream_data`] swaps the whole buffer rather
    /// than editing bytes in place, and nothing in this crate reaches inside
    /// one. Two streams holding one buffer therefore cannot become visible to
    /// each other, and no path needs `Rc::make_mut`'s silent copy-on-write.
    ///
    /// `Rc<Vec<u8>>` rather than `Rc<[u8]>`: `Rc::<[u8]>::from(vec)` cannot
    /// retrofit its refcount header onto an allocation `Vec` already made, so
    /// it memcpys the whole payload (the same trap `page_split`'s
    /// `SharedSource` documents), and every payload arrives as a `Vec`.
    /// `Rc::new(vec)` moves the `Vec`'s three words and copies nothing. That
    /// this happens to be two levels of indirection like `shared_ptr<Buffer>`
    /// is coincidence, not correspondence: qpdf needs a `Buffer` object
    /// because C++ cannot say borrow-or-own in the type system, so `Buffer`
    /// carries a runtime flag for it (`include/qpdf/Buffer.hh:35-46` offers
    /// both an owning and a non-owning constructor). `Rc` rather than `Arc`
    /// because [`ObjectHandle`] itself is `Rc`-based, so this value is `!Send`
    /// regardless.
    Stream {
        stream_dict: ObjectHandle,
        /// `None` means original source bytes; `Some` is replacement data.
        stream_data: Option<Rc<Vec<u8>>>,
        /// A deferred replacement source. This is mutually exclusive with
        /// `stream_data`, matching qpdf's `stream_provider` slot.
        stream_provider: Option<Rc<dyn StreamDataProvider>>,
        /// Parse-time length for the original-source branch. qpdf's
        /// `replaceFilterData` updates `/Length` but not this member
        /// (`libqpdf/QPDF_Stream.cc:668-685`).
        stream_length: usize,
    },
    // qpdf-cutover-delete(flpdf-25kg.3.3): qpdf cannot store an indirect
    // handle as another indirect object's replacement value. Delete this
    // legacy redirect variant after `set_object` and ref-chain consumers move
    // to canonical in-place slot replacement.
    // An indirect object whose own resolved value is *itself* a bare
    // reference to another object (e.g. `4 0 obj\n5 0 R\nendobj`, or a
    // reference redirected in place via `Pdf::set_object`) -- never seen
    // from a file/ObjStm parse (`Pdf::resolve_object_handle`'s native path
    // integerizes a top-level bare reference to `Integer` instead, matching
    // qpdf), but a real value `Pdf::set_object` callers pass directly (used
    // throughout this crate to redirect/collapse holder chains). A child
    // array/dictionary entry that is a reference is represented as a
    // separate indirect `ObjectHandle`, never this variant -- see
    // `Pdf::lift_to_handle` and `materialize`'s own doc.
    Reference(ObjectRef),
}

/// qpdf stores dictionary names in their canonical name-string form, including
/// the leading slash (`QPDFObjectHandle.hh:747-780`; `QPDF_Dictionary.cc:97-153`).
/// The legacy `Object`/`Dictionary` bridge deliberately omits that slash, so
/// conversion happens only at the boundary between the two representations.
pub(crate) fn canonical_dictionary_key(key: &[u8]) -> Vec<u8> {
    if key.first() == Some(&b'/') {
        key.to_vec()
    } else {
        let mut canonical = Vec::with_capacity(key.len() + 1);
        canonical.push(b'/');
        canonical.extend_from_slice(key);
        canonical
    }
}

/// Convert a legacy `Dictionary` key body to qpdf's canonical dictionary key.
/// Legacy keys omit the PDF name delimiter, but their decoded body may itself
/// begin with `/` (for example, the body of `/#2Ffoo`).
pub(crate) fn canonical_dictionary_key_from_legacy(key: &[u8]) -> Vec<u8> {
    let mut canonical = Vec::with_capacity(key.len() + 1);
    canonical.push(b'/');
    canonical.extend_from_slice(key);
    canonical
}

/// Convert a canonical ObjectHandle dictionary key back to the legacy
/// `Dictionary` representation, whose writer adds the leading slash itself.
pub(crate) fn legacy_dictionary_key(key: &[u8]) -> &[u8] {
    key.strip_prefix(b"/").unwrap_or(key)
}

fn canonicalize_object_value(value: ObjectValue) -> ObjectValue {
    match value {
        ObjectValue::Dictionary(entries) => {
            if entries.keys().all(|key| key.starts_with(b"/")) {
                ObjectValue::Dictionary(entries)
            } else {
                ObjectValue::Dictionary(
                    entries
                        .into_iter()
                        .map(|(key, value)| (canonical_dictionary_key(&key), value))
                        .collect(),
                )
            }
        }
        other => other,
    }
}

/// Emit one canonical PDF name token from an ObjectHandle dictionary key.
/// `write_name_escaped` writes only the name body, while the canonical key
/// already contains `/`; stripping it first prevents `//Key` output.
fn write_dictionary_key(out: &mut Vec<u8>, key: &[u8]) {
    out.push(b'/');
    crate::object::write_name_escaped(out, legacy_dictionary_key(key));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ContainmentOwner {
    pdf_unique_id: Option<u64>,
    object_ref: ObjectRef,
}

/// The resolution state of an object handle's uniform backing slot.
///
/// `Missing` and `Resolved(ObjectValue::Null)` are kept as distinct variants
/// even though both currently present the same externally-observable value
/// (`is_null() == true`): the former is a reference absent from — or broken
/// in — the source cross-reference table (`Pdf::resolve_object_handle`'s
/// fallback arm), the latter is a genuinely parsed literal `null` object.
/// Collapsing them into one variant would lose that distinction the moment a
/// later task needs it (e.g. to tell a dangling reference apart from a real
/// null value for diagnostics).
#[derive(Debug)]
pub(crate) enum ObjectState {
    NotYetResolved,
    Resolved(ObjectValue),
    Missing,
    /// qpdf's internal construction sentinel (`ot_reserved`). It is an
    /// indirect, document-owned slot with no serializable `ObjectValue` and
    /// must be replaced before the document is written
    /// (`libqpdf/QPDF_Reserved.cc:1-27`).
    Reserved,
    /// The owning document has been dropped and this slot's value has been
    /// severed (see [`ObjectHandle::disconnect`]). Distinct from `Missing`
    /// (a reference absent from the source) so a future diagnostic can still
    /// tell the two apart. It is also distinct from null: [`ObjectHandle::is_null`]
    /// reports `false`, while value accessors without an error channel retain
    /// their null fallback.
    Destroyed,
}

impl ObjectHandle {
    /// Parse one standalone PDF object without an owning document context.
    ///
    /// This ports `QPDFObjectHandle::parse(string)`: malformed input that
    /// would make qpdf warn is an error here, only C whitespace may trail the
    /// object, and a nested indirect reference is rejected because no
    /// document can canonicalize it. Parse an indirect file object through
    /// [`crate::Pdf`] instead when object-cache identity is required.
    ///
    /// qpdf correspondence: `QPDFObjectHandle::parse`
    /// (`libqpdf/QPDFObjectHandle.cc:1672-1698`) and no-context indirect
    /// reference handling in `QPDFParser::parseRemainder`
    /// (`libqpdf/QPDFParser.cc:135-176`).
    pub fn parse(input: &[u8]) -> Result<Self> {
        crate::parser::parse_explicit_object_handle(input)
    }

    /// True if this handle wraps a value constructed directly, without an
    /// indirect object number/generation.
    pub fn is_direct(&self) -> bool {
        self.0.borrow().object_ref.is_none()
    }

    /// True if this handle refers to an indirect object.
    pub fn is_indirect(&self) -> bool {
        self.0.borrow().object_ref.is_some()
    }

    /// True if this handle is qpdf's internal reserved construction sentinel.
    ///
    /// The sentinel is represented as an `ObjectState` rather than an
    /// `ObjectValue`: it has an indirect identity and document owner, but no
    /// PDF value that can be resolved or serialized
    /// (`include/qpdf/Constants.h:108-127`, `libqpdf/QPDF_Reserved.cc:1-27`).
    pub fn is_reserved(&self) -> bool {
        let state = self.0.borrow().state.clone();
        let reserved = matches!(&*state.borrow(), ObjectState::Reserved);
        reserved
    }

    /// The object number/generation for an indirect handle, or `None` for a
    /// direct one.
    pub fn object_ref(&self) -> Option<ObjectRef> {
        self.0.borrow().object_ref
    }

    /// True if `self` and `other` share the same underlying storage — the
    /// same canonical object, not merely an equal value.
    ///
    /// This is qpdf's `QPDFObjectHandle::isSameObjectAs`: mutations and lazy
    /// resolution observed through either handle affect the same allocation
    /// (`include/qpdf/QPDFObjectHandle.hh:304-309`,
    /// `libqpdf/QPDFObjectHandle.cc:224-227`).
    pub fn is_same_object_as(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    #[cfg(test)]
    fn ptr_eq(&self, other: &Self) -> bool {
        self.is_same_object_as(other)
    }

    // An indirect slot with neither a document identity nor a resolver.
    // Test-only, and necessarily so: every caller is inside a `#[cfg(test)]`
    // module (this file's own tests, plus `parser.rs`'s
    // `handle_path_parity_tests` and `reader.rs`'s tests).
    // `Pdf::get_object_handle` does *not* use this — it needs both the
    // document identity and the resolver, so it calls
    // `new_indirect_for_pdf_with_resolver`.
    #[cfg(test)]
    pub(crate) fn new_indirect_unresolved(object_ref: ObjectRef, offset: i64) -> Self {
        Self::new_indirect_unresolved_with_identity(object_ref, offset, None, None)
    }

    /// Construct a canonical unresolved slot carrying both its owning
    /// document's identity and that document's resolver — what
    /// `Pdf::get_object_handle` needs to hand out.
    ///
    /// Neither half is sufficient alone. The resolver is what
    /// [`Self::try_dereference`] upgrades and calls; the identity is what
    /// [`Self::belongs_to_pdf`] answers on. [`Self::set_resolved`] propagates
    /// that identity separately from the live immediate-parent edges used by
    /// [`Self::containing_object_refs_for_pdf`].
    ///
    /// `pdf_unique_id` itself ports qpdf's document-level unique id:
    /// `QPDF::getUniqueId` (`include/qpdf/QPDF.hh:283`,
    /// `libqpdf/QPDF.cc:2294-2296`) over the member
    /// `unsigned long long unique_id{0}` (`include/qpdf/QPDF.hh:1454`),
    /// minted from a function-static atomic counter in the constructor
    /// (`libqpdf/QPDF.cc:211-212`). `Pdf`'s own `NEXT_PDF_ID.fetch_add`
    /// (`reader.rs:645`) is that same mechanism, serving the same
    /// cross-document identity purpose qpdf uses it for: keying
    /// `m->object_copiers` by the *other* document's id
    /// (`libqpdf/QPDF.cc:2065`, already noted at `reader.rs:82`) and
    /// comparing two documents for sameness
    /// (`libqpdf/QPDFPageObjectHelper.cc:1020`).
    ///
    /// What has no qpdf counterpart is storing that id *per object*. A qpdf
    /// value reaches its document through a raw `QPDF*` back-pointer
    /// (`libqpdf/qpdf/QPDFValue.hh:150`, `QPDF* qpdf{nullptr}`) which
    /// `QPDFObject::doResolve` hands straight to `QPDF::Resolver::resolve`
    /// (`libqpdf/QPDFObject.cc:6-11`), so upstream one pointer is both the
    /// identity and the route to the resolver. This port splits that single
    /// pointer into a plain tag plus a `Weak`, which is why both arguments
    /// have to be supplied together here.
    pub(crate) fn new_indirect_for_pdf_with_resolver(
        object_ref: ObjectRef,
        offset: i64,
        pdf_unique_id: u64,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        Self::new_indirect_unresolved_with_identity(
            object_ref,
            offset,
            Some(pdf_unique_id),
            Some(resolver),
        )
    }

    /// Construct qpdf's document-owned reserved sentinel with a fresh
    /// indirect identity. The resolver link is weak for the same lifetime
    /// reason as ordinary canonical handles.
    pub(crate) fn new_reserved_for_pdf(
        object_ref: ObjectRef,
        pdf_unique_id: u64,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        let handle = Self(Rc::new(RefCell::new(ObjectSlot {
            state: Rc::new(RefCell::new(ObjectState::Reserved)),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: Some(object_ref),
            active_pdf_unique_id: Some(pdf_unique_id),
            resolver: Some(resolver),
            parsed_offset: NO_PARSED_OFFSET,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            containment_parents: Vec::new(),
            description: None,
        })));
        handle.register_state_owner();
        handle
    }

    /// Construct a canonical unresolved slot attached to a document resolver
    /// but initially without active document metadata. The resolver link is
    /// weak so a surviving handle cannot keep its document alive.
    ///
    /// Its resolved direct children keep only weak immediate-parent links.
    /// [`Self::containing_object_refs_for_pdf`] follows those live links at
    /// query time and reads the reached indirect slot's *current* object
    /// reference and active document identity. It neither copies Root
    /// metadata to children nor records a permanent `None` root. Until this
    /// slot is promoted, it has no active identity, so
    /// [`Self::belongs_to_pdf`] is false and owner lookup is empty for every
    /// document. That makes this the narrower test constructor, not the
    /// qpdf-native shape: upstream one `QPDF*` carries both identity and the
    /// resolver, while [`Self::new_indirect_for_pdf_with_resolver`] is what a
    /// handle vended by a `Pdf` needs.
    #[allow(dead_code)] // production QPDF::Resolver wiring is flpdf-25kg.3.5;
                        // this primitive slice exercises the constructor with
                        // sealed resolver unit tests only
    pub(crate) fn new_indirect_with_resolver(
        object_ref: ObjectRef,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        Self::new_indirect_unresolved_with_identity(
            object_ref,
            NO_PARSED_OFFSET,
            None,
            Some(resolver),
        )
    }

    fn new_indirect_unresolved_with_identity(
        object_ref: ObjectRef,
        offset: i64,
        pdf_unique_id: Option<u64>,
        resolver: Option<Weak<dyn DocumentResolver>>,
    ) -> Self {
        let _ = offset;
        let handle = Self(Rc::new(RefCell::new(ObjectSlot {
            state: Rc::new(RefCell::new(ObjectState::NotYetResolved)),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: Some(object_ref),
            active_pdf_unique_id: pdf_unique_id,
            resolver,
            parsed_offset: NO_PARSED_OFFSET,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            containment_parents: Vec::new(),
            description: None,
        })));
        handle.register_state_owner();
        handle
    }

    fn new_direct(value: ObjectValue, parsed_offset: i64) -> Self {
        Self::new_direct_with_resolver(value, parsed_offset, None)
    }

    fn new_direct_with_resolver(
        value: ObjectValue,
        parsed_offset: i64,
        resolver: Option<Weak<dyn DocumentResolver>>,
    ) -> Self {
        let value = canonicalize_object_value(value);
        let handle = Self(Rc::new(RefCell::new(ObjectSlot {
            state: Rc::new(RefCell::new(ObjectState::Resolved(value))),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: None,
            active_pdf_unique_id: None,
            resolver,
            parsed_offset,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            containment_parents: Vec::new(),
            description: None,
        })));
        handle.register_state_owner();
        handle.with_value(|value| {
            if let Some(value) = value {
                handle.attach_value_children(value);
            }
        });
        handle
    }

    fn register_state_owner(&self) {
        let owners = self.0.borrow().state_owners.clone();
        let self_slot = self.0.clone();
        let mut owners = owners.borrow_mut();
        owners.retain(|owner| owner.strong_count() != 0);
        if !owners.iter().any(|owner| {
            owner
                .upgrade()
                .is_some_and(|slot| Rc::ptr_eq(&slot, &self_slot))
        }) {
            owners.push(Rc::downgrade(&self_slot));
        }
    }

    #[allow(dead_code)] // consumed by the staged mutation boundary below
    fn remove_state_owner(
        owners: &Rc<RefCell<Vec<Weak<RefCell<ObjectSlot>>>>>,
        slot_to_remove: &Rc<RefCell<ObjectSlot>>,
    ) {
        let mut owners = owners.borrow_mut();
        owners.retain(|owner| {
            owner
                .upgrade()
                .is_some_and(|slot| !Rc::ptr_eq(&slot, slot_to_remove))
        });
    }

    fn state_owner_handles(&self) -> Vec<Self> {
        let owners = self.0.borrow().state_owners.clone();
        let mut owners = owners.borrow_mut();
        let mut handles = Vec::new();
        owners.retain(|owner| {
            let Some(slot) = owner.upgrade() else {
                return false;
            };
            handles.push(Self(slot));
            true
        });
        handles
    }

    fn detach_child_from_state_owners(&self, child: &ObjectHandle) {
        for owner in self.state_owner_handles() {
            let parent = owner.containment_parent();
            Self::detach_child_from_parent(child, &parent);
        }
    }

    fn attach_child_to_state_owners(&self, child: &ObjectHandle) {
        for owner in self.state_owner_handles() {
            let parent = owner.containment_parent();
            Self::attach_child_to_parent(child, &parent);
        }
    }

    fn state_children(state: &ObjectState) -> Vec<ObjectHandle> {
        match state {
            ObjectState::Resolved(value) => Self::direct_children(value),
            ObjectState::NotYetResolved
            | ObjectState::Missing
            | ObjectState::Reserved
            | ObjectState::Destroyed => Vec::new(),
        }
    }

    /// Replace the shared payload and keep every slot that owns it in sync
    /// with the payload's direct-child containment edges.
    fn replace_shared_state(&self, new_state: ObjectState) -> ObjectState {
        let state = self.0.borrow().state.clone();
        let old_state = {
            let mut state = state.borrow_mut();
            std::mem::replace(&mut *state, new_state)
        };
        let old_children = Self::state_children(&old_state);
        let new_children = {
            let state = state.borrow();
            Self::state_children(&state)
        };
        for owner in self.state_owner_handles() {
            let parent = owner.containment_parent();
            for child in &old_children {
                Self::detach_child_from_parent(child, &parent);
            }
            for child in &new_children {
                Self::attach_child_to_parent(child, &parent);
            }
        }
        old_state
    }

    /// Replace only this slot's whole-object state, leaving other slots that
    /// currently share its payload untouched. qpdf's remove/disconnect
    /// transitions rebind the departing `QPDFObject`; they do not mutate the
    /// `QPDFValue` allocation that a replacement alias still observes.
    fn replace_detached_state(&self, new_state: ObjectState) {
        let old_state = self.0.borrow().state.clone();
        let old_owners = self.0.borrow().state_owners.clone();
        let old_children = {
            let state = old_state.borrow();
            Self::state_children(&state)
        };
        let new_state = Rc::new(RefCell::new(new_state));

        Self::remove_state_owner(&old_owners, &self.0);
        {
            let mut slot = self.0.borrow_mut();
            slot.state = new_state;
            slot.state_owners = Rc::new(RefCell::new(Vec::new()));
        }
        self.register_state_owner();

        let parent = self.containment_parent();
        for child in &old_children {
            Self::detach_child_from_parent(child, &parent);
        }
    }

    /// Make `self` and a distinct direct replacement handle observe one
    /// shared payload, preserving the canonical target slot's identity.
    ///
    /// qpdf's `QPDFObject::assign` shares the `QPDFValue` allocation rather
    /// than copying it (`QPDFObject_private.hh:117-120`). The two Rust
    /// [`ObjectHandle`] slots therefore retain separate object metadata while
    /// sharing the payload and its mutation visibility.
    #[allow(dead_code)] // consumer cutover is flpdf-25kg.3.6.3
    pub(crate) fn share_value_state_with(&self, source: &Self) -> Result<()> {
        if self.is_same_object_as(source) {
            return Ok(());
        }
        if !source.is_direct() {
            return Err(crate::Error::Unsupported(
                "replacement ObjectHandle must be direct".to_string(),
            ));
        }
        let source_state = source.0.borrow().state.clone();
        if !matches!(&*source_state.borrow(), ObjectState::Resolved(_)) {
            return Err(crate::Error::Unsupported(
                "replacement ObjectHandle is not initialized".to_string(),
            ));
        }
        let source_owners = source.0.borrow().state_owners.clone();
        let old_state = {
            let mut target = self.0.borrow_mut();
            let old_state = target.state.clone();
            let old_owners = target.state_owners.clone();
            Self::remove_state_owner(&old_owners, &self.0);
            target.state = source_state.clone();
            target.state_owners = source_owners.clone();
            old_state
        };
        Self::register_state_owner(self);

        let old_children = {
            let state = old_state.borrow();
            Self::state_children(&state)
        };
        let new_children = {
            let state = source_state.borrow();
            Self::state_children(&state)
        };
        let parent = self.containment_parent();
        for child in &old_children {
            Self::detach_child_from_parent(child, &parent);
        }
        for child in &new_children {
            Self::attach_child_to_parent(child, &parent);
        }
        if let Some(pdf_unique_id) = self.0.borrow().active_pdf_unique_id {
            source.associate_pdf_identity(pdf_unique_id, &mut BTreeSet::new());
        }
        Ok(())
    }

    /// Turn an indirect canonical slot into the floating null object qpdf
    /// leaves behind after `removeObject`.
    #[allow(dead_code)] // consumer cutover is flpdf-25kg.3.6.3
    pub(crate) fn remove_from_document(&self) {
        if !self.is_indirect() {
            return;
        }
        self.replace_detached_state(ObjectState::Resolved(ObjectValue::Null));
        let mut slot = self.0.borrow_mut();
        slot.object_ref = None;
        slot.active_pdf_unique_id = None;
        slot.resolver = None;
        slot.description = None;
        slot.parsed_offset = NO_PARSED_OFFSET;
        slot.end_before_space = NO_PARSED_OFFSET;
        slot.end_after_space = NO_PARSED_OFFSET;
    }

    /// Promote this existing uniform slot to an indirect object in place.
    ///
    /// qpdf registers the existing `QPDFObject` in its cache, then gives that
    /// same allocation its object generation and owning document
    /// (`libqpdf/QPDF.cc:1835-1839,1882-1897`); it does not clone a direct
    /// payload during promotion. This is the corresponding primitive here:
    /// every alias keeps its `Rc` identity while this slot receives its active
    /// object reference, document identity, and weak resolver.
    #[allow(dead_code)] // consumer migration in flpdf-25kg.3.6 will use this primitive
    pub(crate) fn promote_to_indirect(
        &self,
        object_ref: ObjectRef,
        pdf_unique_id: u64,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        let children = {
            let mut slot = self.0.borrow_mut();
            slot.object_ref = Some(object_ref);
            slot.active_pdf_unique_id = Some(pdf_unique_id);
            slot.resolver = Some(resolver);
            slot.pdf_unique_ids.insert(pdf_unique_id);
            let state = slot.state.borrow();
            match &*state {
                ObjectState::Resolved(value) => Self::direct_children(value),
                ObjectState::NotYetResolved
                | ObjectState::Missing
                | ObjectState::Reserved
                | ObjectState::Destroyed => Vec::new(),
            }
        };
        let mut visited = BTreeSet::new();
        visited.insert(Rc::as_ptr(&self.0) as usize);
        for child in children {
            child.associate_pdf_identity(pdf_unique_id, &mut visited);
        }
        self.clone()
    }

    /// Construct a direct handle wrapping an already-built [`ObjectValue`], at
    /// the no-offset sentinel. Used at the explicit raw-object materialization
    /// boundary (`Pdf::lift`/`Pdf::lift_to_handle`) to wrap a value lifted from
    /// a legacy [`crate::Object`] without going through one of the typed public
    /// factories above.
    pub(crate) fn from_value(value: ObjectValue) -> Self {
        Self::new_direct(value, NO_PARSED_OFFSET)
    }

    /// Construct a parser-created direct value with the owning document's
    /// weak resolver, matching qpdf's per-value `QPDF*` association. The
    /// resolver is intentionally weak so direct values do not keep the
    /// document alive after parsing (`QPDFValue.hh:60-66,149-152`).
    pub(crate) fn from_value_with_resolver(
        value: ObjectValue,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        Self::new_direct_with_resolver(value, NO_PARSED_OFFSET, Some(resolver))
    }

    /// Consume a directly-constructed, exclusively-owned handle and return
    /// its value and parsed offset without cloning.
    ///
    /// Used by the canonical parser's top-level file-object handle entry point
    /// (`parser::parse_qpdf_direct_object_handle_with_diagnostics`), which builds the
    /// top-level value as a handle purely to reuse the same
    /// offset-assignment machinery as every nested child, then immediately
    /// unwraps it into the pre-existing indirect slot the resolved object
    /// actually belongs to.
    ///
    /// Returns `None` for an indirect handle, or for a direct handle whose
    /// `Rc` is still shared elsewhere (refcount > 1) — the latter cannot
    /// happen for a handle a caller alone constructed and never cloned.
    pub(crate) fn into_direct_value(self) -> Option<(ObjectValue, i64)> {
        if self.0.borrow().object_ref.is_some() {
            return None; // cov:ignore: unreachable per the invariant noted above
        }
        if Rc::strong_count(&self.0) != 1 {
            return None;
        }
        let parent = Rc::downgrade(&self.0);
        let children = {
            let slot = self.0.borrow();
            let state = slot.state.borrow();
            match &*state {
                ObjectState::Resolved(value) => Self::direct_children(value),
                ObjectState::NotYetResolved
                | ObjectState::Missing
                | ObjectState::Reserved
                | ObjectState::Destroyed => return None,
            }
        };
        for child in children {
            Self::detach_child_from_parent(&child, &parent);
        }
        let slot = Rc::try_unwrap(self.0).ok()?.into_inner();
        let state = Rc::try_unwrap(slot.state).ok()?.into_inner();
        match state {
            ObjectState::Resolved(value) => Some((value, slot.parsed_offset)),
            ObjectState::NotYetResolved
            | ObjectState::Missing
            | ObjectState::Reserved
            | ObjectState::Destroyed => None, // cov:ignore: sole-owner branch just observed Resolved and no alias can mutate it
        }
    }

    /// Legacy cloning helper for the unchanged public
    /// `Pdf::make_indirect_object_handle` allocator: returns a direct value
    /// copy, or `None` for an indirect handle. It is not the qpdf-native
    /// promotion primitive — qpdf promotes by registering and updating the
    /// existing `QPDFObject` allocation (`libqpdf/QPDF.cc:1835-1839,1882-1897`).
    /// Consumer migration to [`Self::promote_to_indirect`] is scheduled in
    /// `flpdf-25kg.3.6`.
    ///
    /// qpdf shares the *whole* `QPDFObject`: `QPDF::makeIndirectObject`
    /// registers `oh.getObj()`, the caller's existing allocation, under a
    /// fresh `QPDFObjGen` (`libqpdf/QPDF.cc:1883-1898`), so an edit through
    /// either handle is one edit to one stream. This helper cannot do that
    /// while the allocator mints a separate slot, and a *partial* share is
    /// worse than none: `stream_dict` is an `ObjectHandle` (shared
    /// mutability) while `stream_data` is a per-value field, so sharing the
    /// dictionary alone would let a later [`Self::replace_stream_data`]
    /// rewrite one stream's `/Length`/`/Filter`/`/DecodeParms` while
    /// swapping the other's bytes — the promoted object would describe
    /// payload it does not hold. Until whole-object promotion lands, the
    /// stream dictionary is privatized like any other direct child so each
    /// slot stays internally consistent; the payload `Rc` is shared, which
    /// is safe because replacing it swaps a field rather than mutating the
    /// buffer.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::shallow_copy`]'s stream rejection when the stream
    /// dictionary being privatized itself holds a *direct* stream, the same
    /// case `QPDF_Dictionary::copy` throws on.
    pub(crate) fn direct_value_clone(&self) -> Result<Option<ObjectValue>> {
        let slot = self.0.borrow();
        if slot.object_ref.is_some() {
            return Ok(None);
        }
        let state = slot.state.borrow();
        match &*state {
            ObjectState::Resolved(value) => Ok(Some(match value {
                ObjectValue::Stream {
                    stream_dict,
                    stream_data,
                    stream_provider,
                    stream_length,
                } => ObjectValue::Stream {
                    stream_dict: shallow_copy_child(stream_dict)?,
                    stream_data: stream_data.clone(),
                    stream_provider: stream_provider.clone(),
                    stream_length: *stream_length,
                },
                other => other.clone(),
            })),
            ObjectState::NotYetResolved
            | ObjectState::Missing
            | ObjectState::Reserved
            | ObjectState::Destroyed => Ok(None),
        }
    }

    /// Copy the source description and parsed offset that belong to this
    /// direct slot onto a freshly allocated indirect slot. qpdf promotion
    /// registers the existing `QPDFObject` allocation, so these metadata
    /// fields remain attached to the promoted value
    /// (`libqpdf/QPDF.cc:1882-1898`).
    pub(crate) fn copy_description_and_parsed_offset_to(&self, target: &Self) {
        let (description, parsed_offset, end_before_space, end_after_space) = {
            let slot = self.0.borrow();
            (
                slot.description.clone(),
                slot.parsed_offset,
                slot.end_before_space,
                slot.end_after_space,
            )
        };
        let mut target_slot = target.0.borrow_mut();
        target_slot.description = description;
        if target_slot.parsed_offset < 0 {
            target_slot.parsed_offset = parsed_offset;
        }
        target_slot.end_before_space = end_before_space;
        target_slot.end_after_space = end_after_space;
    }

    /// Mark this indirect handle's value as resolved to `value`. A no-op for
    /// a direct handle, which has no resolution state to update.
    pub(crate) fn set_resolved(&self, value: ObjectValue) {
        if self.is_indirect() {
            self.replace_shared_state(ObjectState::Resolved(canonicalize_object_value(value)));
        }
    }

    /// Return the canonical indirect objects that contain this direct handle.
    /// Indirect handles own themselves and are intentionally excluded: callers
    /// that need that case already use [`Self::object_ref`].
    #[cfg(test)]
    pub(crate) fn containing_object_refs(&self) -> Vec<ObjectRef> {
        self.containment_roots()
            .into_iter()
            .map(|owner| owner.object_ref)
            .collect()
    }

    pub(crate) fn containing_object_refs_for_pdf(&self, pdf_unique_id: u64) -> Vec<ObjectRef> {
        self.containment_roots()
            .into_iter()
            .filter(|owner| owner.pdf_unique_id == Some(pdf_unique_id))
            .map(|owner| owner.object_ref)
            .collect()
    }

    pub(crate) fn belongs_to_pdf(&self, pdf_unique_id: u64) -> bool {
        let slot = self.0.borrow();
        if slot.object_ref.is_some() {
            slot.active_pdf_unique_id == Some(pdf_unique_id)
        } else {
            slot.pdf_unique_ids.is_empty() || slot.pdf_unique_ids.contains(&pdf_unique_id)
        }
    }

    /// qpdf's `checkOwnership` rejects a value that is already attached to a
    /// different document, including an indirect child nested below an
    /// otherwise direct replacement value. A direct value with no recorded
    /// document identity is unowned and can be inserted into one document.
    #[allow(dead_code)] // consumer cutover is flpdf-25kg.3.6.3
    pub(crate) fn belongs_exclusively_to_pdf(&self, pdf_unique_id: u64) -> bool {
        let mut pending = vec![self.clone()];
        let mut visited = BTreeSet::new();
        while let Some(handle) = pending.pop() {
            let identity = Rc::as_ptr(&handle.0) as usize;
            if !visited.insert(identity) {
                continue;
            }
            let (object_ref, active_pdf_unique_id, pdf_unique_ids, children) = {
                let slot = handle.0.borrow();
                let state = slot.state.borrow();
                (
                    slot.object_ref,
                    slot.active_pdf_unique_id,
                    slot.pdf_unique_ids.clone(),
                    Self::state_children(&state),
                )
            };
            if object_ref.is_some() {
                if active_pdf_unique_id != Some(pdf_unique_id) {
                    return false;
                }
                continue;
            }
            if pdf_unique_ids
                .iter()
                .any(|known_pdf_unique_id| *known_pdf_unique_id != pdf_unique_id)
            {
                return false;
            }
            pending.extend(children);
        }
        true
    }

    /// Mark this indirect handle as resolved-to-null because its reference is
    /// absent from — or broken in — the source cross-reference table (see
    /// [`ObjectState`]). A no-op for a direct handle, which has no
    /// resolution state to update.
    ///
    /// Also resets the parsed offset to the no-offset sentinel: "An absent,
    /// freed, dangling, cyclic, or otherwise unresolvable indirect object
    /// retains its indirect identity but resolves to null with parsed offset
    /// `-1`" (design, Parsed-Offset Contract). Without this, a handle that
    /// was previously resolved (e.g. natively parsed with a real offset)
    /// and later marked missing — [`crate::Pdf::delete_object`] on an
    /// already-resolved handle — would keep reporting its former body's
    /// source position even though the value now reads as null.
    /// The parsed description is discarded with the same transition, so an
    /// outstanding handle cannot keep attributing warnings to the deleted
    /// value's source location.
    pub(crate) fn set_missing(&self) {
        if self.is_indirect() {
            self.replace_detached_state(ObjectState::Missing);
            let mut slot = self.0.borrow_mut();
            slot.parsed_offset = NO_PARSED_OFFSET;
            slot.end_before_space = NO_PARSED_OFFSET;
            slot.end_after_space = NO_PARSED_OFFSET;
            slot.description = None;
        }
    }

    /// Sever this indirect handle's resolved value, dropping any `ObjectHandle`
    /// children it holds. A no-op for a direct handle.
    ///
    /// A resolved indirect value can hold direct-owning [`ObjectHandle`]
    /// children (array/dictionary/stream-dict entries) that are themselves
    /// indirect handles sharing the same canonical `Rc` identity as this
    /// document's registry entries. Two objects that reference each other
    /// (e.g. a `/Pages` node and a page's `/Parent`, both common in real
    /// PDFs) therefore form a strong reference cycle once both are resolved,
    /// which `Rc` alone never collects.
    ///
    /// Mirrors qpdf's own teardown: `QPDF::~QPDF()` walks its object cache,
    /// disconnects every cached indirect object, including unresolved entries,
    /// and replaces only non-null values with `QPDF_Destroyed()`, specifically
    /// to break cycles like this one
    /// (`libqpdf/QPDF.cc`, `QPDF::~QPDF`). Literal null and missing values stay
    /// null. The reader's `Pdf::drop` calls this for every entry in its handle
    /// registry — the sole owner of the canonical `Rc`s — before the registry
    /// itself is dropped, so no lingering cycle keeps a document's object
    /// graph (and any reachable stream buffers) alive past the `Pdf` that
    /// produced it.
    ///
    /// Resets the parsed offset to the no-offset sentinel only when the value
    /// is destroyed. Surviving null and missing values retain their existing
    /// parsed-offset provenance.
    pub(crate) fn disconnect(&self) {
        let should_destroy = {
            let slot = self.0.borrow();
            if slot.object_ref.is_none() {
                return;
            }
            let state = slot.state.borrow();
            !matches!(
                &*state,
                ObjectState::Resolved(ObjectValue::Null) | ObjectState::Missing
            )
        };
        if should_destroy {
            self.replace_detached_state(ObjectState::Destroyed);
        }
        let mut slot = self.0.borrow_mut();
        slot.object_ref = None;
        slot.active_pdf_unique_id = None;
        slot.resolver = None;
        if should_destroy {
            slot.description = None;
            slot.parsed_offset = NO_PARSED_OFFSET;
            slot.end_before_space = NO_PARSED_OFFSET;
            slot.end_after_space = NO_PARSED_OFFSET;
        }
    }

    /// The `Rc` strong count backing this handle's identity. Test-only:
    /// lets a regression test prove a cycle-breaking fix actually frees the
    /// `Rc`s involved, without exposing reference counting as production API.
    #[cfg(test)]
    pub(crate) fn strong_count(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    /// True if this handle's value is known without performing resolution: a
    /// direct handle always is; an indirect handle is once it has left its
    /// initial state, whether that landed on a real value, on a reference
    /// that turned out to be missing from the source, or on a value severed
    /// because its owning document was dropped.
    pub fn is_resolved(&self) -> bool {
        let state = self.0.borrow().state.clone();
        let resolved = !matches!(*state.borrow(), ObjectState::NotYetResolved);
        resolved
    }

    /// Resolve this handle's own canonical slot in place, mirroring
    /// `QPDFObjectHandle::dereference` → `QPDFObject::resolve`.
    ///
    /// Direct and already-terminal handles are no-ops. An unresolved handle
    /// whose document has been dropped returns an error and stays unresolved.
    pub(crate) fn try_dereference(&self) -> Result<()> {
        let (object_ref, resolver) = {
            let slot = self.0.borrow();
            let Some(object_ref) = slot.object_ref else {
                return Ok(());
            };
            let state = slot.state.borrow();
            if !matches!(&*state, ObjectState::NotYetResolved) {
                return Ok(());
            }
            (object_ref, slot.resolver.clone())
        };

        let Some(resolver) = resolver.and_then(|resolver| resolver.upgrade()) else {
            return Err(Error::Internal(format!(
                "object {} {} belongs to a dropped PDF",
                object_ref.number, object_ref.generation
            )));
        };
        resolver.resolve_indirect(object_ref, self)
    }

    /// The document that owns this handle, qpdf's `QPDF* context`.
    ///
    /// `QPDFValue::qpdf` is set by the description machinery
    /// (`libqpdf/qpdf/QPDFValue.hh:60-83`), so upstream a direct object
    /// parsed out of a file carries a context as well as an indirect one.
    /// Live parser direct values and canonical indirect slots carry that
    /// weak resolver here; programmatic direct handles and
    /// `QPDFObjectHandle::parse`-equivalent explicit parses remain
    /// contextless.
    #[allow(dead_code)] // reached through the try_* accessors, whose own
                        // production consumers land with flpdf-25kg.3.6
    pub(crate) fn context(&self) -> Option<Rc<dyn DocumentResolver>> {
        let slot = self.0.borrow();
        slot.resolver
            .as_ref()
            .and_then(Weak::upgrade)
            .or_else(|| {
                if let Some(ObjectDescription::Child(child)) = &slot.description {
                    child
                        .parent
                        .upgrade()
                        .and_then(|p| p.borrow().resolver.as_ref().and_then(Weak::upgrade))
                } else {
                    None
                }
            })
            .or_else(|| {
                slot.containment_parents.iter().find_map(|parent| {
                    parent
                        .upgrade()
                        .and_then(|p| p.borrow().resolver.as_ref().and_then(Weak::upgrade))
                })
            })
    }

    #[allow(dead_code)]
    pub(crate) fn set_description_json(&self, input: String, object: String, offset: i64) {
        let mut slot = self.0.borrow_mut();
        slot.description = Some(ObjectDescription::Json(JsonDescription { input, object }));
        // qpdf writes the description offset through the same set-once guard
        // as any other parsed offset (`QPDFValue::setDescription` calls
        // `setParsedOffset`, `libqpdf/qpdf/QPDFValue.hh:60-65,90-100`), so a
        // value that already carries an operational offset (e.g. a stream's
        // encoded-data start) keeps it.
        if slot.parsed_offset < 0 {
            slot.parsed_offset = offset;
        }
    }

    /// Emit `message` through this handle's context, or report it as the
    /// error qpdf throws when there is none.
    ///
    /// Ports `QPDFObjectHandle::warn`
    /// (`libqpdf/QPDFObjectHandle.cc:2385-2396`), whose contextless arm is
    /// `throw e`. `QPDFExc` derives from `std::runtime_error`
    /// (`include/qpdf/QPDFExc.hh:29`), which this crate classifies as
    /// [`crate::Error::System`]. With an empty filename,
    /// `QPDFExc::createWhat` (`libqpdf/QPDFExc.cc:19-49`) prefixes a non-empty
    /// object description and renders a bare message only when that
    /// description is empty; this port forms the same prefix before the
    /// contextless error is returned.
    #[allow(dead_code)] // same deferred consumers as `context`
    fn warn_through_context(&self, message: String) -> Result<()> {
        match self.context() {
            Some(context) => context.warn(message),
            None => Err(Error::System(message)),
        }
    }

    pub(crate) fn description(&self) -> String {
        self.0.borrow().get_description()
    }

    /// Return the unrendered qpdf-style template, when this handle carries
    /// one. Canonical resolver transfers must copy this representation rather
    /// than the rendered [`Self::description`]: a caller's escaped literal
    /// `$PO`/`$OG` would otherwise become parser-owned placeholders again on
    /// the next render.
    pub(crate) fn description_template(&self) -> Option<String> {
        match self.0.borrow().description.as_ref() {
            Some(ObjectDescription::Template(template)) => Some(template.clone()),
            Some(ObjectDescription::Json(_) | ObjectDescription::Child(_)) | None => None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_description(&self, description: String, offset: i64) {
        let mut slot = self.0.borrow_mut();
        slot.description = Some(ObjectDescription::Template(description));
        // Set-once, matching qpdf's `setParsedOffset` guard that
        // `QPDFValue::setDescription` calls through
        // (`libqpdf/qpdf/QPDFValue.hh:60-65,90-100`). `QPDF_Stream::setDescription`
        // delegates to the same guarded setter (`QPDF_Stream.cc:299-302`), so a
        // stream's already-recorded encoded-data start survives a later
        // description assignment rather than being overwritten by the
        // description's own offset.
        if slot.parsed_offset < 0 {
            slot.parsed_offset = offset;
        }
    }

    /// Clear source-description metadata when the handle's value is replaced
    /// by caller-supplied data. The replacement no longer belongs to the
    /// source location that produced the old description, so the indirect
    /// object fallback (`object N G`) must be used instead.
    pub(crate) fn clear_description(&self) {
        self.0.borrow_mut().description = None;
    }

    #[allow(dead_code)]
    pub(crate) fn set_child_description(
        &self,
        parent: &ObjectHandle,
        static_descr: &str,
        var_descr: &str,
    ) {
        let mut slot = self.0.borrow_mut();
        slot.description = Some(ObjectDescription::Child(ChildDescription {
            parent: Rc::downgrade(&parent.0),
            static_descr: static_descr.to_owned(),
            var_descr: var_descr.to_owned(),
        }));
    }

    /// Report that an accessor expecting `expected_type` ran on this handle.
    ///
    /// Ports `QPDFObjectHandle::typeWarning`
    /// (`libqpdf/QPDFObjectHandle.cc:2168-2189`). qpdf dereferences first and
    /// throws `std::logic_error` when it cannot; [`Self::try_dereference`]
    /// already returns [`crate::Error::Internal`], this crate's counterpart,
    /// for the one state that cannot resolve.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::System`] when no owning document is reachable,
    /// mirroring the exception qpdf throws instead of warning.
    #[allow(dead_code)] // same deferred consumers as `context`
    pub(crate) fn type_warning(&self, expected_type: &str, warning: &str) -> Result<()> {
        self.try_dereference()?;
        let desc = self.description();
        let prefix = if desc.is_empty() {
            String::new()
        } else {
            format!("{desc}: ")
        };
        self.warn_through_context(format!(
            "{prefix}operation for {expected_type} attempted on object of type {}: {warning}",
            self.type_name()
        ))
    }

    /// Report damage this handle noticed about itself.
    ///
    /// Ports `QPDFObjectHandle::warnIfPossible`
    /// (`libqpdf/QPDFObjectHandle.cc:2191-2201`). Its guard is
    /// `dereference() && obj->getDescription(context, description)`, and that
    /// second call returns `qpdf != nullptr`
    /// (`libqpdf/qpdf/QPDFObject_private.hh:94-100`), so the else-branch is
    /// exactly the no-context case. Unlike [`Self::type_warning`] it writes
    /// the bare message to the process-global default logger's error stream
    /// and returns normally rather than reporting an error.
    ///
    /// A handle whose document has been dropped is this port's counterpart of
    /// that null context, so the context is tested before resolution is
    /// attempted: dereferencing first would turn the dropped document into a
    /// resolution error and lose the branch. Any *other* resolution failure
    /// still propagates rather than being reported as a contextless warning.
    ///
    /// # Errors
    ///
    /// Propagates a resolution failure from a document that is still
    /// reachable, and reports a sink that refuses the message.
    #[allow(dead_code)] // same deferred consumers as `context`
    pub(crate) fn warn_if_possible(&self, warning: &str) -> Result<()> {
        if let Some(context) = self.context() {
            self.try_dereference()?;
            let desc = self.description();
            let prefix = if desc.is_empty() {
                String::new()
            } else {
                format!("{desc}: ")
            };
            context.warn(format!("{prefix}{warning}"))
        } else {
            crate::QPDFLogger::default_logger().error(format!("{warning}\n"))
        }
    }

    /// Report an object-level problem whose message qpdf passes through
    /// unchanged.
    ///
    /// Ports `QPDFObjectHandle::objectWarning`
    /// (`libqpdf/QPDFObjectHandle.cc:2203-2212`). No type name is interposed,
    /// and — unlike [`Self::type_warning`] — qpdf performs no dereference
    /// here, because the callers that reach it have already type-checked.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::System`] when no owning document is reachable,
    /// mirroring the exception qpdf throws instead of warning.
    #[allow(dead_code)] // same deferred consumers as `context`
    pub(crate) fn object_warning(&self, warning: &str) -> Result<()> {
        let desc = self.description();
        let prefix = if desc.is_empty() {
            String::new()
        } else {
            format!("{desc}: ")
        };
        self.warn_through_context(format!("{prefix}{warning}"))
    }

    /// qpdf-compatible null inspection with lazy dereference.
    pub(crate) fn try_is_null(&self) -> Result<bool> {
        self.try_dereference()?;
        Ok(self.is_null())
    }

    /// qpdf-compatible dictionary inspection with lazy dereference.
    ///
    /// Ports `QPDFObjectHandle::asDictionary`, the silent internal helper the
    /// dictionary accessors branch on. It raises no warning of its own; the
    /// warning belongs to [`Self::try_get_key`] and [`Self::try_get_keys`],
    /// which is where qpdf places it.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_dictionary(
        &self,
    ) -> Result<Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>>> {
        self.try_dereference()?;
        Ok(self.as_dictionary())
    }

    /// Return the sorted qpdf-canonical keys whose values do not lazily resolve
    /// to null. Each returned key is a decoded PDF name string including its
    /// leading `/`, such as `/Type`; the set is sorted lexicographically.
    ///
    /// Ports `QPDF_Dictionary::getKeys` and its `QPDFObjectHandle::getKeys`
    /// delegation (`libqpdf/QPDF_Dictionary.cc:117-127`;
    /// `libqpdf/QPDFObjectHandle.cc:997-1009`). The dictionary snapshot is
    /// owned before child resolution, so no container borrow crosses a
    /// resolver call.
    ///
    /// A non-dictionary receiver yields an empty set after raising qpdf's
    /// `typeWarning("dictionary", "treating as empty")` at `:1000`.
    ///
    /// # Errors
    ///
    /// Propagates resolution failures.
    #[allow(dead_code)] // consumed by flpdf-h8mv after this prerequisite lands
    pub(crate) fn try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>> {
        self.try_dereference()?;
        let Some(entries) = self.as_dictionary() else {
            self.type_warning("dictionary", "treating as empty")?;
            return Ok(BTreeSet::new());
        };
        let mut result = BTreeSet::new();
        for (key, child) in entries {
            if !child.try_is_null()? {
                result.insert(key);
            }
        }
        Ok(result)
    }

    /// qpdf-compatible name inspection with lazy dereference.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_name(&self) -> Result<Option<Vec<u8>>> {
        self.try_dereference()?;
        Ok(self.as_name())
    }

    /// True when this handle lazily resolves to the requested decoded name.
    ///
    /// Ports `QPDFObjectHandle::isNameAndEquals`
    /// (`libqpdf/QPDFObjectHandle.cc:456-459`). qpdf's canonical name string
    /// includes its leading slash; [`ObjectValue::Name`] follows this crate's
    /// existing representation and stores the same decoded bytes without it.
    #[allow(dead_code)] // consumed by flpdf-25kg.3.12 after this prerequisite lands
    pub(crate) fn try_is_name_and_equals(&self, name: &[u8]) -> Result<bool> {
        self.try_dereference()?;
        Ok(self.with_value(
            |value| matches!(value, Some(ObjectValue::Name(actual)) if actual.as_slice() == name),
        ))
    }

    /// True when this handle is the requested decoded name or an array with a
    /// matching name item.
    ///
    /// Ports `QPDFObjectHandle::isOrHasName` in its exact short-circuit order:
    /// inspect the holder as a name first, then inspect array items one at a
    /// time (`libqpdf/QPDFObjectHandle.cc:1027-1039`). Each array borrow ends
    /// before the selected child is resolved.
    #[allow(dead_code)] // consumed by flpdf-25kg.3.12 after this prerequisite lands
    pub(crate) fn try_is_or_has_name(&self, name: &[u8]) -> Result<bool> {
        if self.try_is_name_and_equals(name)? {
            return Ok(true);
        }
        let Some(count) = self.try_array_len()? else {
            return Ok(false);
        };
        for index in 0..count {
            if self
                .try_array_item(index)?
                .map(|item| item.try_is_name_and_equals(name))
                .transpose()?
                .unwrap_or(false)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// True when this handle lazily resolves to a dictionary whose optional
    /// `/Type` and `/Subtype` names equal the requested decoded bytes.
    ///
    /// Ports `QPDFObjectHandle::isDictionaryOfType` and its left-to-right
    /// short-circuiting (`libqpdf/QPDFObjectHandle.cc:461-466`). Dictionary
    /// keys use qpdf's canonical leading slash; the requested type names
    /// remain decoded name bytes without it, such as
    /// `CryptFilterDecodeParms`.
    #[allow(dead_code)] // consumed by flpdf-25kg.3.12 after this prerequisite lands
    pub(crate) fn try_is_dictionary_of_type(
        &self,
        type_name: &[u8],
        subtype_name: &[u8],
    ) -> Result<bool> {
        self.try_dereference()?;
        let is_dictionary =
            self.with_value(|value| matches!(value, Some(ObjectValue::Dictionary(_))));
        if !is_dictionary {
            return Ok(false);
        }
        if !type_name.is_empty()
            && !self
                .try_get_key(b"/Type")?
                .try_is_name_and_equals(type_name)?
        {
            return Ok(false);
        }
        if !subtype_name.is_empty()
            && !self
                .try_get_key(b"/Subtype")?
                .try_is_name_and_equals(subtype_name)?
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// qpdf-compatible array inspection with lazy dereference. Only the array
    /// itself is resolved; each returned child keeps its own identity.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_array(&self) -> Result<Option<Vec<ObjectHandle>>> {
        self.try_dereference()?;
        Ok(self.as_array())
    }

    /// qpdf-compatible array *length* with lazy dereference — the item count
    /// without materializing the items.
    ///
    /// `QPDFObjectHandle::getArrayNItems` is `asArray()->size()`
    /// (`libqpdf/QPDFObjectHandle.cc:758-768`), and `asArray` is
    /// `return dereference() ? obj->as<QPDF_Array>() : nullptr;` — a borrowed
    /// `QPDF_Array*`, not a copy (`libqpdf/QPDFObjectHandle.cc:252-256`), so
    /// qpdf reads the length in place. `QPDF_Stream::filterable` uses that to
    /// size its `/Filter` and `/DecodeParms` loops
    /// (`libqpdf/QPDF_Stream.cc:398`, `:443`, `:447`) before touching a single
    /// item. [`Self::try_as_array`]
    /// cannot serve that caller: it snapshots the child vector, so a length
    /// that is only going to be rejected still costs a `Vec` allocation and
    /// one `Rc` clone per child.
    ///
    /// **Deliberately not qpdf's non-array answer.** `getArrayNItems` warns
    /// `typeWarning("array", "treating as empty")` and returns 0 for a
    /// non-array (`libqpdf/QPDFObjectHandle.cc:763-766`), so qpdf reads a
    /// non-array as an empty one. This returns `None`, matching
    /// [`Self::try_as_array`], and leaves the meaning of "not an array" to the
    /// caller — for [`crate::stream_filter::decode_filter_specs_from_handle`]
    /// that is the "stream filter type is not name or array" error. That
    /// divergence predates this accessor and is not widened by it; folding
    /// qpdf's treat-as-empty in here would silently turn a rejected `/Filter`
    /// into an accepted unfiltered stream.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_array_len(&self) -> Result<Option<usize>> {
        self.try_dereference()?;
        Ok(self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.len()),
            _ => None,
        }))
    }

    /// Return one live child handle from a lazily resolved array.
    ///
    /// This is the valid-index portion of `QPDFObjectHandle::getArrayItem`
    /// (`libqpdf/QPDFObjectHandle.cc:770-775`). qpdf borrows its `QPDF_Array`
    /// and copies one `QPDFObjectHandle`; this port briefly borrows the backing
    /// `Vec` and clones one `Rc`-backed handle. The child is not resolved.
    ///
    /// qpdf warns and returns a special null for a non-array or out-of-bounds
    /// index (`:776-785`). This prerequisite's consumer only calls after both
    /// arrays have equal, known lengths, so invalid-domain diagnostics remain
    /// outside this method and are represented as `None` rather than guessed.
    #[allow(dead_code)] // consumed by flpdf-25kg.3.12 after this prerequisite lands
    pub(crate) fn try_array_item(&self, index: usize) -> Result<Option<ObjectHandle>> {
        self.try_dereference()?;
        Ok(self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => children.get(index).cloned(),
            _ => None,
        }))
    }

    /// qpdf-compatible integer inspection with lazy dereference.
    ///
    /// Ports `QPDFObjectHandle::asInteger`, the silent internal helper.
    /// [`Self::try_get_int_value`] is the accessor that warns.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_as_integer(&self) -> Result<Option<i64>> {
        self.try_dereference()?;
        Ok(self.as_integer())
    }

    /// This handle's integer value, warning and yielding `0` for any other
    /// type.
    ///
    /// Ports `QPDFObjectHandle::getIntValue`
    /// (`libqpdf/QPDFObjectHandle.cc:502-513`).
    ///
    /// # Errors
    ///
    /// Propagates resolution failures, and — for a receiver with no reachable
    /// document — the error [`Self::type_warning`] reports in place of the
    /// warning.
    #[allow(dead_code)] // same deferred consumers as `context`
    pub(crate) fn try_get_int_value(&self) -> Result<i64> {
        match self.try_as_integer()? {
            Some(value) => Ok(value),
            None => {
                self.type_warning("integer", "returning 0")?;
                Ok(0)
            }
        }
    }

    /// [`Self::try_get_int_value`] saturated into `i32`, warning at each
    /// clamp.
    ///
    /// Ports `QPDFObjectHandle::getIntValueAsInt`
    /// (`libqpdf/QPDFObjectHandle.cc:525-543`). The comparisons are strict,
    /// so `i32::MIN` and `i32::MAX` themselves pass through unwarned.
    ///
    /// # Errors
    ///
    /// As [`Self::try_get_int_value`]. A clamp warning also goes through
    /// [`Self::warn_if_possible`], which — unlike [`Self::type_warning`] —
    /// usually reports no error of its own; but a *reachable* document whose
    /// warning sink itself fails (no default logger sink, a resolver with no
    /// warn receiver) still propagates that failure here, and the saturated
    /// value is not returned in that case.
    #[allow(dead_code)] // same deferred consumers as `context`
    pub(crate) fn try_get_int_value_as_int(&self) -> Result<i32> {
        let value = self.try_get_int_value()?;
        if value < i64::from(i32::MIN) {
            self.warn_if_possible("requested value of integer is too small; returning INT_MIN")?;
            Ok(i32::MIN)
        } else if value > i64::from(i32::MAX) {
            self.warn_if_possible("requested value of integer is too big; returning INT_MAX")?;
            Ok(i32::MAX)
        } else {
            Ok(value as i32)
        }
    }

    /// qpdf-compatible dictionary lookup. The holder dictionary is resolved;
    /// the returned child retains its own direct/indirect identity.
    ///
    /// Ports `QPDFObjectHandle::getKey`
    /// (`libqpdf/QPDFObjectHandle.cc:978-989`). A non-dictionary receiver
    /// yields null. qpdf additionally raises
    /// `typeWarning("dictionary", "returning null for attempted key
    /// retrieval")` at `:984`, and gives its null a child description naming
    /// the key.
    ///
    /// `key` must be qpdf's decoded, canonical dictionary key including its
    /// leading `/` (for example, `/Type`). Lookup is exact; a slashless key is
    /// not an alias and is treated as missing.
    ///
    /// # Errors
    ///
    /// Propagates resolution failures.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_get_key(&self, key: &[u8]) -> Result<ObjectHandle> {
        self.try_dereference()?;
        let (is_dictionary, child) = self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => (true, entries.get(key).cloned()),
            _ => (false, None),
        });
        if let Some(child) = child {
            Ok(child)
        } else {
            if !is_dictionary {
                self.type_warning("dictionary", "returning null for attempted key retrieval")?;
            }
            let key_str = String::from_utf8_lossy(key);
            let null = ObjectHandle::null();
            let var_descr = if key_str.starts_with('/') {
                key_str.into_owned()
            } else {
                format!("/{key_str}")
            };
            null.set_child_description(self, " -> dictionary key $VD", &var_descr);
            Ok(null)
        }
    }

    /// qpdf-compatible visible-key test. `key` must be qpdf's decoded,
    /// canonical dictionary key including its leading `/` (for example,
    /// `/Type`). Lookup is exact; a slashless key is not an alias. A present
    /// value that resolves to null is treated as absent, matching
    /// `QPDF_Dictionary::hasKey`.
    #[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
    pub(crate) fn try_has_key(&self, key: &[u8]) -> Result<bool> {
        self.try_dereference()?;
        let child = self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
            _ => None,
        });
        match child {
            Some(child) => Ok(!child.try_is_null()?),
            None => Ok(false),
        }
    }

    /// Construct a direct integer value.
    pub fn integer(value: i64) -> Self {
        Self::new_direct(ObjectValue::Integer(value), NO_PARSED_OFFSET)
    }

    /// The qpdf-compatible signed parsed offset. `-1` means the value was
    /// not parsed from a source position (`QPDFObjectHandle::getParsedOffset`,
    /// `include/qpdf/QPDFObjectHandle.hh:415-419`).
    pub fn get_parsed_offset(&self) -> i64 {
        self.0.borrow().parsed_offset
    }

    // Record `offset` as the parsed offset, but only if none has been set
    // yet (matches qpdf: "set only while still negative",
    // `libqpdf/qpdf/QPDFValue.hh:90-100`). The live parser wires up callers;
    // this remains exposed so this module's own tests can exercise the
    // set-once contract independently.
    #[allow(dead_code)]
    pub(crate) fn set_parsed_offset_if_unset(&self, offset: i64) {
        let mut slot = self.0.borrow_mut();
        if slot.parsed_offset < 0 {
            slot.parsed_offset = offset;
        }
    }

    /// Record qpdf's source extent metadata updated alongside a cache value by
    /// `QPDF::updateCache` (`libqpdf/QPDF.cc:1843-1858`). These offsets are
    /// distinct from the value's parsed/token offset: they bracket the
    /// indirect object's `endobj` token and the whitespace following it.
    pub(crate) fn set_end_offsets(&self, end_before_space: i64, end_after_space: i64) {
        let mut slot = self.0.borrow_mut();
        slot.end_before_space = end_before_space;
        slot.end_after_space = end_after_space;
    }

    /// Return qpdf's cached source extent metadata for this value.
    pub(crate) fn end_offsets(&self) -> (i64, i64) {
        let slot = self.0.borrow();
        (slot.end_before_space, slot.end_after_space)
    }

    /// Construct a direct null value.
    pub fn null() -> Self {
        Self::new_direct(ObjectValue::Null, NO_PARSED_OFFSET)
    }

    /// Construct a direct boolean value.
    pub fn boolean(value: bool) -> Self {
        Self::new_direct(ObjectValue::Boolean(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct real (floating-point) value.
    pub fn real(value: f64) -> Self {
        Self::new_direct(ObjectValue::Real(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct name value.
    pub fn name(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Name(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct string value.
    pub fn string(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::String(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct content-stream operator token value.
    pub fn operator(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Operator(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct raw inline-image byte payload value.
    pub fn inline_image(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::InlineImage(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct array value. Child values are handles, so cloning
    /// or re-reading this array's children never deep-copies their subtrees.
    pub fn array(children: Vec<ObjectHandle>) -> Self {
        Self::new_direct(ObjectValue::Array(children), NO_PARSED_OFFSET)
    }

    /// Construct a direct dictionary value from `entries`. Iteration order
    /// is the lexicographic order of the keys, not insertion order (matching
    /// [`crate::Dictionary`]); a repeated key keeps its last value. Values
    /// are handles, so cloning or re-reading this dictionary's entries never
    /// deep-copies their subtrees. Each input key is normalized to qpdf's
    /// decoded canonical name string, including the leading `/`.
    pub fn dictionary(entries: Vec<(Vec<u8>, ObjectHandle)>) -> Self {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (canonical_dictionary_key(&key), value))
            .collect();
        Self::new_direct(ObjectValue::Dictionary(entries), NO_PARSED_OFFSET)
    }

    /// Construct a direct stream value from `dict` (a dictionary handle —
    /// typically built via [`Self::dictionary`]) and `data` (the stream's
    /// raw, undecoded bytes). qpdf's own model never allows a stream to be a
    /// direct value (only ever a top-level indirect object,
    /// `libqpdf/QPDF_Stream.cc:173-178`); this crate's own types do not
    /// forbid it, matching [`Self::unparse_resolved`]'s own doc for that
    /// case. Mainly useful for building a handle that is deliberately never
    /// attached to a [`crate::Pdf`]'s object graph, e.g. in tests.
    ///
    /// `data` is used as given rather than copied, the way
    /// `QPDFObjectHandle::newStream(QPDF*, std::shared_ptr<Buffer>)` uses "the
    /// given buffer as the stream data"
    /// (`include/qpdf/QPDFObjectHandle.hh:546-558`). Handing the same buffer
    /// to a second stream shares it; nothing here copies the bytes.
    pub fn stream(dict: ObjectHandle, data: Rc<Vec<u8>>) -> Self {
        Self::new_direct(
            ObjectValue::Stream {
                stream_dict: dict,
                stream_data: Some(data),
                stream_provider: None,
                stream_length: 0,
            },
            NO_PARSED_OFFSET,
        )
    }

    /// Construct a direct real value that preserves a non-canonical source
    /// literal (e.g. `.4`) alongside its parsed value, mirroring
    /// [`crate::Object::RealLiteral`], so that a real number written in the
    /// source PDF unparses byte-identically. `literal` is expected to parse
    /// back to `value` and to differ from `value`'s canonical string form —
    /// see [`crate::Object::RealLiteral`]'s own documented invariant.
    pub fn real_literal(value: f64, literal: Vec<u8>) -> Self {
        Self::new_direct(
            ObjectValue::RealLiteral { value, literal },
            NO_PARSED_OFFSET,
        )
    }

    /// The value as an `f64`/literal-bytes pair if this handle's value — its
    /// own if direct, or its already-resolved value if indirect — is a real
    /// value with a preserved source literal, or `None` otherwise. This
    /// never performs resolution itself: an indirect handle that has not
    /// yet been resolved returns `None` too, the same as a resolved value
    /// of a different type.
    pub fn as_real_literal(&self) -> Option<(f64, Vec<u8>)> {
        self.with_value(|value| match value {
            Some(ObjectValue::RealLiteral { value, literal }) => Some((*value, literal.clone())),
            _ => None,
        })
    }

    /// The value as `bool` if this handle's value — its own if direct, or
    /// its already-resolved value if indirect — is a boolean, or `None`
    /// otherwise. Never performs resolution itself.
    pub fn as_boolean(&self) -> Option<bool> {
        self.with_value(|value| match value {
            Some(ObjectValue::Boolean(b)) => Some(*b),
            _ => None,
        })
    }

    /// The value as `f64` if this handle's value — its own if direct, or
    /// its already-resolved value if indirect — is a real number (including
    /// one with a preserved non-canonical source literal), or `None`
    /// otherwise. Mirrors [`crate::Object::as_real`]'s own real-or-real-literal
    /// arm. Never performs resolution itself.
    pub fn as_real(&self) -> Option<f64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Real(v) | ObjectValue::RealLiteral { value: v, .. }) => Some(*v),
            _ => None,
        })
    }

    /// The value as decoded PDF name bytes if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is a name, or
    /// `None` otherwise. Never performs resolution itself.
    pub fn as_name(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The value as string bytes if this handle's value — its own if
    /// direct, or its already-resolved value if indirect — is a string, or
    /// `None` otherwise. Never performs resolution itself.
    pub fn as_string(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The target as an indirect-object reference if this handle's value —
    /// its own if direct, or its already-resolved value if indirect — is
    /// itself a bare reference (e.g. one redirected in place to another
    /// object via `Pdf::set_object`), mirroring [`crate::Object::Reference`],
    /// or `None` otherwise. This is distinct from an indirect *child*
    /// handle, which is exposed via [`Self::is_indirect`]/[`Self::object_ref`]
    /// on the child handle itself rather than through this accessor. Never
    /// performs resolution itself.
    pub fn as_reference(&self) -> Option<ObjectRef> {
        self.with_value(|value| match value {
            Some(ObjectValue::Reference(object_ref)) => Some(*object_ref),
            _ => None,
        })
    }

    /// True if this handle's value is known to be null. An indirect handle
    /// whose value has not yet been resolved returns `false` — this method
    /// never performs resolution itself, so an unresolved handle is not
    /// assumed to be null. Once resolved, this reflects the real value:
    /// `true` both for a genuinely parsed `null` object and for a reference
    /// that turned out to be missing from the source. A handle disconnected
    /// when its owning document is dropped is `Destroyed`, not null.
    pub fn is_null(&self) -> bool {
        let state = self.0.borrow().state.clone();
        let is_null = matches!(
            &*state.borrow(),
            ObjectState::Resolved(ObjectValue::Null) | ObjectState::Missing
        );
        is_null
    }

    /// True if this indirect handle resolved as a missing or malformed source
    /// object rather than as a parsed literal null. The distinction is kept
    /// private to the canonical reader/consumer boundary because both states
    /// intentionally present as null through the public qpdf-compatible view.
    pub(crate) fn is_missing(&self) -> bool {
        let state = self.0.borrow().state.clone();
        let is_missing = matches!(&*state.borrow(), ObjectState::Missing);
        is_missing
    }

    /// The value as `i64` if this handle's value — its own if direct, or its
    /// already-resolved value if indirect — is an integer, or `None`
    /// otherwise. This never performs resolution itself: an indirect handle
    /// that has not yet been resolved returns `None` too, the same as a
    /// resolved value of a different type.
    pub fn as_integer(&self) -> Option<i64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Integer(n)) => Some(*n),
            _ => None,
        })
    }

    /// The child handles if this handle's value — its own if direct, or its
    /// already-resolved value if indirect — is an array, or `None`
    /// otherwise. This never performs resolution itself: an indirect handle
    /// that has not yet been resolved returns `None` too, the same as a
    /// resolved value of a different type. Cloning the returned `Vec` clones
    /// only the child `Rc` handles, not their subtrees.
    pub fn as_array(&self) -> Option<Vec<ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.clone()),
            _ => None,
        })
    }

    /// The entries if this handle's value — its own if direct, or its
    /// already-resolved value if indirect — is a dictionary, or `None`
    /// otherwise. This never performs resolution itself: an indirect handle
    /// that has not yet been resolved returns `None` too, the same as a
    /// resolved value of a different type. Cloning the returned map clones
    /// only the child `Rc` handles, not their subtrees. Dictionary keys use
    /// qpdf's decoded canonical name strings, including the leading `/`.
    pub fn as_dictionary(&self) -> Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => Some(entries.clone()),
            _ => None,
        })
    }

    /// The value at `key` if this handle's value is a dictionary and `key`
    /// is present, or a direct null handle otherwise (a missing key, or
    /// this handle not being a dictionary at all) — mirrors
    /// `QPDFObjectHandle::getKey`'s own "returns null for a missing key or
    /// a non-dictionary handle" contract (`libqpdf/QPDFObjectHandle.cc:979-988`).
    /// Unlike
    /// [`Self::as_dictionary`], this never snapshots the whole dictionary —
    /// it returns the one live child handle directly, so a caller that only
    /// needs one key does not pay for every sibling. `key` must be qpdf's
    /// decoded, canonical dictionary key including its leading `/` (for
    /// example, `/Type`); lookup is exact and slashless keys are missing.
    /// Never performs resolution itself.
    pub fn get_key(&self, key: &[u8]) -> ObjectHandle {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
            _ => None,
        })
        .unwrap_or_else(ObjectHandle::null)
    }

    /// True if this handle's value is a dictionary that has `key`, distinct
    /// from [`Self::get_key`] returning a null handle for `key` (which
    /// cannot tell a missing key apart from one whose value is genuinely
    /// null) — mirrors `QPDFObjectHandle::hasKey`
    /// (`libqpdf/QPDFObjectHandle.cc:966-976`). `key` must be qpdf's decoded,
    /// canonical dictionary key including its leading `/`; lookup is exact and
    /// slashless keys are absent. `false` for a non-dictionary handle. Never
    /// performs resolution itself.
    pub fn has_key(&self, key: &[u8]) -> bool {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => entries.contains_key(key),
            _ => false,
        })
    }

    /// Insert or overwrite `key` in this handle's dictionary with `value`,
    /// mutating the live value every other clone of this handle also
    /// observes — mirrors `QPDFObjectHandle::replaceKey`
    /// (`libqpdf/QPDFObjectHandle.cc:1199-1209`) and
    /// `QPDF_Dictionary::replaceKey`
    /// (`libqpdf/QPDF_Dictionary.cc:135-153`). A direct null removes the
    /// key, while an indirect null or dangling indirect reference is
    /// retained as the dictionary value. A no-op on a
    /// non-dictionary handle or an unresolved/missing/destroyed indirect
    /// handle, matching qpdf's own `typeWarning`-and-ignore contract rather
    /// than panicking. Also a no-op if `value` is a direct handle sharing
    /// `self`'s value state — inserting it into the dictionary would
    /// otherwise create a direct cycle that none of this crate's recursive
    /// walkers (`shallow_copy`, `materialize`, `Debug`) guard against, since
    /// they only stop recursion at an indirect-handle boundary. This does
    /// not detect a multi-hop reciprocal cycle built from two or more
    /// `replace_key` calls across distinct direct dictionaries. Unlike
    /// qpdf's `replaceKey`, this does not check that `value` belongs to the
    /// same document (`checkOwnership`) — no caller in this crate crosses
    /// document boundaries this way today. Never performs resolution
    /// itself. `key` must be qpdf's decoded, canonical dictionary key including
    /// its leading `/`; this API does not normalize slashless input.
    ///
    /// This mutates the live handle graph directly. If `self`'s ref has
    /// already been read through [`crate::Pdf::resolve`] or
    /// [`crate::Pdf::resolve_borrowed`], those methods cache the
    /// materialized value the first time a ref is resolved and do not
    /// re-derive it — a later call to either will keep returning the
    /// pre-mutation value for that ref rather than observing this change.
    /// Callers that need `resolve`/`resolve_borrowed` to reflect a
    /// mutation made through this API must not have resolved the same ref
    /// through them first.
    ///
    /// This also has no path to inform the owning [`crate::Pdf`] that
    /// `self`'s value changed. After mutating a handle, call
    /// [`crate::Pdf::mark_object_handle_dirty`] with `self`. That marks the
    /// handle itself when it is an indirect object, or its containing indirect
    /// owner(s) when it is a direct child. For an already-registered indirect
    /// handle, [`crate::Pdf::mark_object_dirty`] with the same ref remains the
    /// equivalent lower-level operation.
    pub fn replace_key(&self, key: &[u8], value: ObjectHandle) {
        if value.is_direct() && value.is_null() {
            self.remove_key(key);
            return;
        }
        if self.is_direct_value_alias(&value) {
            return;
        }
        let replaced = self.with_value_mut(|v| {
            if let Some(ObjectValue::Dictionary(entries)) = v {
                return Some(entries.insert(key.to_vec(), value.clone()));
            }
            None
        });
        if let Some(old_value) = replaced {
            if let Some(old_value) = old_value {
                self.detach_child_from_state_owners(&old_value);
            }
            self.attach_child_to_state_owners(&value);
        }
    }

    /// Set one item in the live array, porting qpdf's `setArrayItem`
    /// (`libqpdf/QPDFObjectHandle.cc:871-883`). The receiver is dereferenced
    /// before its array type is inspected, so an unresolved indirect holder
    /// is mutated through the same canonical slot that every alias observes.
    ///
    /// qpdf checks the index before `QPDF_Array::checkOwnership`, warns and
    /// leaves the array unchanged for an invalid index, and otherwise throws
    /// a logic error for a foreign or destroyed item. `Error::Internal` is the
    /// crate's logic-error boundary. A contextless qpdf warning is likewise
    /// returned as the existing `type_warning`/`object_warning` error.
    /// For process safety, flpdf rejects a direct item whose direct-child graph
    /// already reaches this array with [`Error::Internal`]. qpdf's `setAt`
    /// does not detect that cycle, but qpdf's later `makeDirect` traversal does
    /// reject loops with a visited set (`libqpdf/QPDFObjectHandle.cc:2091-2133`).
    /// This remains public because qpdf exposes the same live mutation on
    /// `QPDFObjectHandle`; external canonical consumers must not replace an
    /// indirect array through its parent dictionary or materialize it into
    /// the legacy [`crate::Object`] model.
    ///
    /// As with [`Self::replace_key`], this mutates the live handle graph but
    /// cannot notify the owning [`crate::Pdf`]. After mutating, call
    /// [`crate::Pdf::mark_object_handle_dirty`] with this handle so the
    /// canonical writer observes the change. For an indirect array this
    /// marks its own object reference; for a direct child array it marks the
    /// containing indirect owner(s). [`crate::Pdf::mark_object_dirty`] with
    /// an indirect array's reference is the equivalent lower-level operation.
    pub fn set_array_item(&self, index: usize, value: ObjectHandle) -> Result<()> {
        if !self.prepare_array_mutation("ignoring attempt to set item")? {
            return Ok(());
        }

        let in_bounds = self.with_value(
            |current| matches!(current, Some(ObjectValue::Array(items)) if index < items.len()),
        );
        if !in_bounds {
            return self.object_warning("ignoring attempt to set out of bounds array item");
        }

        if self.would_create_direct_cycle(&value) {
            return Err(Self::direct_cycle_error());
        }

        self.check_array_item_ownership(&value)?;
        let old_value = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(items)) = current else {
                return None; // cov:ignore: prepare_array_mutation fixed the type
            };
            Some(std::mem::replace(&mut items[index], value.clone()))
        });
        if let Some(old_value) = old_value {
            self.detach_child_from_state_owners(&old_value);
            self.attach_child_to_state_owners(&value);
        }
        Ok(())
    }

    /// Replace the live array contents in qpdf's `setFromVector` order
    /// (`libqpdf/QPDFObjectHandle.cc:884-893`, `libqpdf/QPDF_Array.cc:220-243`).
    /// The old contents are detached before the first ownership check. Items
    /// are then checked and attached one at a time, so an ownership error at
    /// item `n` intentionally leaves the accepted prefix in place, matching
    /// qpdf's non-transactional `resize(0)` plus `push_back` loop.
    /// A direct replacement that would make the array graph cyclic returns
    /// [`Error::Internal`] as the flpdf process-safety boundary; qpdf's
    /// `setFromVector` itself checks ownership only.
    /// As with [`Self::replace_key`], call
    /// [`crate::Pdf::mark_object_handle_dirty`] with this handle after the
    /// mutation. The helper marks this array when it is indirect, or its
    /// containing indirect owner(s) when it is a direct child.
    pub fn set_array_items(&self, items: Vec<ObjectHandle>) -> Result<()> {
        if !self.prepare_array_mutation("ignoring attempt to replace items")? {
            return Ok(());
        }
        if items
            .iter()
            .any(|item| self.would_create_direct_cycle(item))
        {
            return Err(Self::direct_cycle_error());
        }

        let expected_len = items.len();
        let old_items = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(current_items)) = current else {
                return None; // cov:ignore: prepare_array_mutation fixed the type
            };
            let old_items = std::mem::take(current_items);
            current_items.reserve(expected_len);
            Some(old_items)
        });
        let Some(old_items) = old_items else {
            return Ok(()); // cov:ignore: prepare_array_mutation fixed the type
        };
        for old_item in old_items {
            self.detach_child_from_state_owners(&old_item);
        }

        for item in items {
            self.check_array_item_ownership(&item)?;
            let child = item.clone();
            let inserted = self.with_value_mut(|current| {
                let Some(ObjectValue::Array(current_items)) = current else {
                    return false; // cov:ignore: only this method can change the state
                };
                current_items.push(item);
                true
            });
            if inserted {
                self.attach_child_to_state_owners(&child);
            }
        }
        Ok(())
    }

    /// Insert one item at an inclusive array position, porting qpdf's
    /// `insertItem` (`libqpdf/QPDFObjectHandle.cc:895-907`). Position `size`
    /// is the append position; larger positions warn without checking item
    /// ownership or changing the array. A direct item whose descendants
    /// already reach this array returns [`Error::Internal`] to keep recursive
    /// live-handle walkers terminating; qpdf's `insert` does not perform this
    /// cycle check.
    /// As with [`Self::replace_key`], call
    /// [`crate::Pdf::mark_object_handle_dirty`] with this handle after the
    /// mutation. The helper marks this array when it is indirect, or its
    /// containing indirect owner(s) when it is a direct child.
    pub fn insert_array_item(&self, index: usize, value: ObjectHandle) -> Result<()> {
        if !self.prepare_array_mutation("ignoring attempt to insert item")? {
            return Ok(());
        }

        let in_bounds = self.with_value(
            |current| matches!(current, Some(ObjectValue::Array(items)) if index <= items.len()),
        );
        if !in_bounds {
            return self.object_warning("ignoring attempt to insert out of bounds array item");
        }

        if self.would_create_direct_cycle(&value) {
            return Err(Self::direct_cycle_error());
        }

        self.check_array_item_ownership(&value)?;
        let child = value.clone();
        let inserted = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(items)) = current else {
                return false; // cov:ignore: prepare_array_mutation fixed the type
            };
            if index == items.len() {
                items.push(value);
            } else {
                items.insert(index, value);
            }
            true
        });
        if inserted {
            self.attach_child_to_state_owners(&child);
        }
        Ok(())
    }

    /// qpdf's `insertItemAndGetNew`: return the supplied handle after the
    /// same insertion/warning/ownership path as [`Self::insert_array_item`].
    /// A direct-cycle rejection is propagated as [`Error::Internal`], so no
    /// handle is returned for a mutation that was not inserted.
    pub fn insert_array_item_and_get_new(
        &self,
        index: usize,
        value: ObjectHandle,
    ) -> Result<ObjectHandle> {
        self.insert_array_item(index, value.clone())?;
        Ok(value)
    }

    /// Append one item to the live array, porting qpdf's `appendItem`
    /// (`libqpdf/QPDFObjectHandle.cc:916-925`, `libqpdf/QPDF_Array.cc:300-313`).
    /// A direct item whose descendants already reach this array returns
    /// [`Error::Internal`] to keep recursive live-handle walkers terminating;
    /// qpdf's `push_back` checks ownership but does not perform this cycle
    /// check.
    /// As with [`Self::replace_key`], call
    /// [`crate::Pdf::mark_object_handle_dirty`] with this handle after the
    /// mutation. The helper marks this array when it is indirect, or its
    /// containing indirect owner(s) when it is a direct child.
    pub fn append_array_item(&self, value: ObjectHandle) -> Result<()> {
        if !self.prepare_array_mutation("ignoring attempt to append item")? {
            return Ok(());
        }
        if self.would_create_direct_cycle(&value) {
            return Err(Self::direct_cycle_error());
        }

        self.check_array_item_ownership(&value)?;
        let child = value.clone();
        let appended = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(items)) = current else {
                return false; // cov:ignore: prepare_array_mutation fixed the type
            };
            items.push(value);
            true
        });
        if appended {
            self.attach_child_to_state_owners(&child);
        }
        Ok(())
    }

    /// qpdf's `appendItemAndGetNew`: return the supplied handle after the
    /// same append/warning/ownership path as [`Self::append_array_item`].
    /// A direct-cycle rejection is propagated as [`Error::Internal`], so no
    /// handle is returned for a mutation that was not appended.
    pub fn append_array_item_and_get_new(&self, value: ObjectHandle) -> Result<ObjectHandle> {
        self.append_array_item(value.clone())?;
        Ok(value)
    }

    /// Erase one live array item, porting qpdf's `eraseItem`
    /// (`libqpdf/QPDFObjectHandle.cc:934-946`).
    /// As with [`Self::replace_key`], call
    /// [`crate::Pdf::mark_object_handle_dirty`] with this handle after the
    /// mutation. The helper marks this array when it is indirect, or its
    /// containing indirect owner(s) when it is a direct child.
    pub fn erase_array_item(&self, index: usize) -> Result<()> {
        self.erase_array_item_and_get_old(index).map(|_| ())
    }

    /// Erase one live array item and return its original handle, porting
    /// qpdf's `eraseItemAndGetOld` (`libqpdf/QPDFObjectHandle.cc:948-955`).
    /// Invalid positions and non-array receivers return a fresh null after
    /// emitting the corresponding qpdf warning when the handle has document
    /// warning context. A direct/contextless handle cannot route that warning
    /// and therefore returns the existing `Error::System` boundary instead.
    /// As with [`Self::replace_key`], call
    /// [`crate::Pdf::mark_object_handle_dirty`] with this handle after the
    /// mutation. The helper marks this array when it is indirect, or its
    /// containing indirect owner(s) when it is a direct child.
    pub fn erase_array_item_and_get_old(&self, index: usize) -> Result<ObjectHandle> {
        if !self.prepare_array_mutation("ignoring attempt to erase item")? {
            return Ok(ObjectHandle::null());
        }

        let in_bounds = self.with_value(
            |current| matches!(current, Some(ObjectValue::Array(items)) if index < items.len()),
        );
        if !in_bounds {
            self.object_warning("ignoring attempt to erase out of bounds array item")?;
            return Ok(ObjectHandle::null());
        }

        let old_value = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(items)) = current else {
                return None; // cov:ignore: prepare_array_mutation fixed the type
            };
            Some(items.remove(index))
        });
        let Some(old_value) = old_value else {
            return Ok(ObjectHandle::null()); // cov:ignore: prepare_array_mutation fixed the type
        };
        self.detach_child_from_state_owners(&old_value);
        Ok(old_value)
    }

    /// Resolve the receiver and emit qpdf's type warning when it is not an
    /// array. This is deliberately separate from `with_value_mut`: the
    /// latter remains a no-hidden-I/O helper for legacy mutation paths, while
    /// qpdf's public array mutators call `asArray()` and therefore resolve
    /// their holder first.
    #[allow(dead_code)] // consumed by the array mutator family above
    fn prepare_array_mutation(&self, warning: &str) -> Result<bool> {
        self.try_dereference()?;
        if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
            return Ok(true);
        }
        self.type_warning("array", warning)?;
        Ok(false)
    }

    /// Port `QPDF_Array::checkOwnership` (`libqpdf/QPDF_Array.cc:10-26`) at
    /// the Rust error boundary. An indirect slot's active PDF is authoritative;
    /// a direct value can retain one or more propagated owner ids through live
    /// containment. A direct value with no owner id is qpdf's unowned array
    /// case and is accepted by the upstream check.
    #[allow(dead_code)] // consumed by the array mutator family above
    fn check_array_item_ownership(&self, item: &ObjectHandle) -> Result<()> {
        let item_is_destroyed = {
            let slot = item.0.borrow();
            let state = slot.state.borrow();
            let destroyed = matches!(&*state, ObjectState::Destroyed);
            destroyed
        };
        if item_is_destroyed {
            return Err(Error::Internal(
                "Attempting to add an uninitialized object to a QPDF_Array.".to_owned(),
            ));
        }

        let owner_pdf_ids = {
            let slot = self.0.borrow();
            match slot.active_pdf_unique_id {
                Some(pdf_unique_id) => vec![pdf_unique_id],
                None => slot.pdf_unique_ids.iter().copied().collect::<Vec<_>>(),
            }
        };
        if owner_pdf_ids
            .iter()
            .any(|pdf_unique_id| !item.belongs_exclusively_to_pdf(*pdf_unique_id))
        {
            return Err(Error::Internal(
                "Attempting to add an object from a different QPDF. Use QPDF::copyForeignObject to add objects from another file.".to_owned(),
            ));
        }
        Ok(())
    }

    /// Replace an existing array item with `value`, preserving `value`'s
    /// shared handle identity. Returns `false` when this handle is not an
    /// array or `index` is out of bounds.
    pub(crate) fn replace_array_item(&self, index: usize, value: ObjectHandle) -> bool {
        if self.would_create_direct_cycle(&value) {
            return false; // cov:ignore: exercised by replace_array_item_preserves_identity_and_rejects_invalid_slots but attributed to closure setup
        }
        let old_value = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(items)) = current else {
                return None; // cov:ignore: exercised by replace_array_item_preserves_identity_and_rejects_invalid_slots but attributed to closure setup
            };
            let Some(item) = items.get_mut(index) else {
                return None; // cov:ignore: exercised by replace_array_item_preserves_identity_and_rejects_invalid_slots but attributed to closure setup
            };
            Some(std::mem::replace(item, value.clone()))
        });
        if let Some(old_value) = old_value {
            self.detach_child_from_state_owners(&old_value);
            self.attach_child_to_state_owners(&value);
            true
        } else {
            false
        }
    }

    /// Replace every item in this live array while preserving the array
    /// handle itself. Returns `false` for a non-array handle or when the
    /// replacement would create a direct value-alias cycle.
    #[allow(dead_code)] // retained for the canonical array-mutation primitive and its tests
    pub(crate) fn replace_array_items(&self, items: Vec<ObjectHandle>) -> bool {
        if items
            .iter()
            .any(|item| self.would_create_direct_cycle(item))
        {
            return false; // cov:ignore: internal callers only replay materialized child arrays
        }
        let old_items = self.with_value_mut(|current| {
            let Some(ObjectValue::Array(current_items)) = current else {
                return None; // cov:ignore: internal callers confirm the array type first
            };
            Some(std::mem::replace(current_items, items.clone()))
        });
        let Some(old_items) = old_items else {
            return false; // cov:ignore: internal callers confirm the array type first
        };
        for item in old_items {
            self.detach_child_from_state_owners(&item);
        }
        for item in &items {
            self.attach_child_to_state_owners(item);
        }
        true
    }

    /// True if `other` is a direct handle sharing this handle's value state.
    /// A direct alias of an indirect container is still a direct cycle when
    /// inserted into that container, while an indirect child remains a legal
    /// PDF reference boundary for recursive walks.
    fn is_direct_value_alias(&self, other: &Self) -> bool {
        if !other.is_direct() {
            return false;
        }
        let self_state = self.0.borrow().state.clone();
        let other_state = other.0.borrow().state.clone();
        Rc::ptr_eq(&self_state, &other_state)
    }

    fn direct_cycle_error() -> Error {
        Error::Internal("attempted to create a direct object cycle".to_owned())
    }

    /// Return whether inserting `candidate` as a direct child would make a
    /// direct-child path reach `self`. Indirect handles are recursion
    /// boundaries in the same way they are for materialization and unparse,
    /// so an indirect candidate or an indirect descendant is not traversed.
    /// The visited set also makes this guard total if a pre-existing direct
    /// cycle came from a dictionary path or an internal caller.
    fn would_create_direct_cycle(&self, candidate: &Self) -> bool {
        if !candidate.is_direct() {
            return false;
        }

        let target_state = self.0.borrow().state.clone();
        let target_id = Rc::as_ptr(&target_state) as usize;
        let mut pending = vec![candidate.clone()];
        let mut visited = BTreeSet::new();

        while let Some(handle) = pending.pop() {
            let state = handle.0.borrow().state.clone();
            let identity = Rc::as_ptr(&state) as usize;
            if identity == target_id {
                return true;
            }
            if !visited.insert(identity) {
                continue;
            }
            let children = match &*state.borrow() {
                ObjectState::Resolved(value) => Self::direct_children(value),
                ObjectState::NotYetResolved
                | ObjectState::Missing
                | ObjectState::Reserved
                | ObjectState::Destroyed => Vec::new(),
            };
            pending.extend(children.into_iter().filter(|child| child.is_direct()));
        }

        false
    }

    fn direct_children(value: &ObjectValue) -> Vec<ObjectHandle> {
        match value {
            ObjectValue::Array(children) => children.clone(),
            ObjectValue::Dictionary(entries) => entries.values().cloned().collect(),
            ObjectValue::Stream { stream_dict, .. } => vec![stream_dict.clone()],
            _ => Vec::new(),
        }
    }

    fn containment_parent(&self) -> Weak<RefCell<ObjectSlot>> {
        Rc::downgrade(&self.0)
    }

    fn same_containment_parent(
        left: &Weak<RefCell<ObjectSlot>>,
        right: &Weak<RefCell<ObjectSlot>>,
    ) -> bool {
        Weak::ptr_eq(left, right)
    }

    fn attach_child_to_parent(child: &ObjectHandle, parent: &Weak<RefCell<ObjectSlot>>) {
        if child.is_indirect() {
            return;
        }
        {
            let mut slot = child.0.borrow_mut();
            slot.containment_parents
                .retain(Self::containment_parent_is_live);
            slot.containment_parents.push(parent.clone());
        }

        let pdf_unique_ids = parent
            .upgrade()
            .map(|parent| {
                let slot = parent.borrow();
                slot.pdf_unique_ids
                    .iter()
                    .copied()
                    .chain(slot.active_pdf_unique_id)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        for pdf_unique_id in pdf_unique_ids {
            child.associate_pdf_identity(pdf_unique_id, &mut BTreeSet::new());
        }
    }

    fn containment_parent_is_live(parent: &Weak<RefCell<ObjectSlot>>) -> bool {
        Weak::strong_count(parent) != 0
    }

    fn detach_child_from_parent(child: &ObjectHandle, parent: &Weak<RefCell<ObjectSlot>>) {
        let mut slot = child.0.borrow_mut();
        if let Some(index) = slot
            .containment_parents
            .iter()
            .position(|candidate| Self::same_containment_parent(candidate, parent))
        {
            slot.containment_parents.remove(index);
        }
    }

    fn attach_value_children(&self, value: &ObjectValue) {
        let parent = self.containment_parent();
        for child in Self::direct_children(value) {
            Self::attach_child_to_parent(&child, &parent);
        }
    }

    fn associate_pdf_identity(&self, pdf_unique_id: u64, visited: &mut BTreeSet<usize>) {
        let mut pending = vec![self.clone()];
        while let Some(handle) = pending.pop() {
            if handle.is_indirect() {
                continue;
            }
            let identity = Rc::as_ptr(&handle.0) as usize;
            if !visited.insert(identity) {
                continue;
            }
            let children = {
                let mut slot = handle.0.borrow_mut();
                slot.pdf_unique_ids.insert(pdf_unique_id);
                let state = slot.state.clone();
                drop(slot);
                let children = match &*state.borrow() {
                    ObjectState::Resolved(value) => Self::direct_children(value),
                    ObjectState::NotYetResolved
                    | ObjectState::Missing
                    | ObjectState::Reserved
                    | ObjectState::Destroyed => Vec::new(),
                };
                children
            };
            pending.extend(children);
        }
    }

    fn containment_roots(&self) -> BTreeSet<ContainmentOwner> {
        if self.is_indirect() {
            return BTreeSet::new();
        }
        let mut roots = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut pending = vec![self.clone()];
        while let Some(handle) = pending.pop() {
            let identity = Rc::as_ptr(&handle.0) as usize;
            if !visited.insert(identity) {
                continue;
            }
            let (object_ref, pdf_unique_id, parents) = {
                let mut slot = handle.0.borrow_mut();
                slot.containment_parents
                    .retain(Self::containment_parent_is_live);
                (
                    slot.object_ref,
                    slot.active_pdf_unique_id,
                    slot.containment_parents.clone(),
                )
            };
            if let Some(object_ref) = object_ref {
                roots.insert(ContainmentOwner {
                    pdf_unique_id,
                    object_ref,
                });
                continue;
            }
            pending.extend(
                parents
                    .into_iter()
                    .filter_map(|parent| parent.upgrade())
                    .map(ObjectHandle),
            );
        }
        roots
    }

    /// Remove `key` from this handle's dictionary if present, mutating the
    /// live value every other clone of this handle also observes — mirrors
    /// `QPDFObjectHandle::removeKey` (`libqpdf/QPDFObjectHandle.cc:1226-1234`).
    /// A no-op if `key` is absent, this handle is not a dictionary, or the
    /// indirect handle is unresolved/missing/destroyed. `key` must be qpdf's
    /// decoded, canonical dictionary key including its leading `/`; this API
    /// does not normalize slashless input. Never performs resolution itself.
    ///
    /// See [`Self::replace_key`]'s doc comment for the same
    /// `resolve`/`resolve_borrowed` staleness caveat and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too.
    pub fn remove_key(&self, key: &[u8]) {
        let removed = self.with_value_mut(|v| {
            if let Some(ObjectValue::Dictionary(entries)) = v {
                return entries.remove(key);
            }
            None
        });
        if let Some(removed) = removed {
            self.detach_child_from_state_owners(&removed);
        }
    }

    /// A fresh, direct handle with a value copied from `self` — mirrors
    /// `QPDFObjectHandle::shallowCopy` (`libqpdf/QPDFObjectHandle.cc:2073-2079`,
    /// which defers to each type's own `copy(shallow=false)` default —
    /// `libqpdf/QPDF_Dictionary.cc`/`libqpdf/QPDF_Array.cc`). Despite the
    /// name, this recursively copies through every *direct* array/dictionary
    /// descendant (each direct child is itself shallow-copied), stopping
    /// only at an *indirect* child, which keeps its existing shared
    /// identity rather than being copied — "shallow" describes not
    /// resolving/duplicating through indirection, not a single-level-only
    /// copy. A scalar value is cloned outright. Always returns a direct
    /// handle regardless of whether `self` is indirect. Never performs
    /// resolution itself: shallow-copying an unresolved/missing/destroyed
    /// indirect handle produces a direct null handle, matching every other
    /// accessor's "no hidden I/O" rule.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] for a stream, whether it is this handle's
    /// own value or a *direct* descendant reached by the recursion.
    /// `QPDF_Stream::copy` (`libqpdf/QPDF_Stream.cc:140-145`) ignores its
    /// `shallow` argument and unconditionally throws
    /// `std::runtime_error("stream objects cannot be cloned")`, so both
    /// `shallowCopy` and `unsafeShallowCopy` refuse a stream; a direct
    /// stream nested in a copied container throws from the same place,
    /// since `QPDF_Dictionary::copy`/`QPDF_Array::copy` call `shallowCopy`
    /// on each direct child. qpdf's own supported way to copy a stream is
    /// `QPDFObjectHandle::copyStream` (`libqpdf/QPDFObjectHandle.cc:2136-2151`),
    /// which mints a *new* stream object and backs it with the source's
    /// buffer instead of duplicating this one in place; the buffer-sharing
    /// half of that is available here through [`Self::replace_stream_data`].
    pub fn shallow_copy(&self) -> Result<ObjectHandle> {
        stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
            self.with_value(|value| match value {
                Some(v) => Ok(ObjectHandle::from_value(shallow_copy_value(v)?)),
                None => Ok(ObjectHandle::null()),
            })
        })
    }

    /// Create a new stream in this handle's owning document, copying its
    /// dictionary across indirect boundaries and retaining its data through
    /// qpdf's buffer/provider source boundary. This is
    /// `QPDFObjectHandle::copyStream` (`libqpdf/QPDFObjectHandle.cc:2136-2151`):
    /// indirect dictionary entries keep their identity, while direct entries
    /// are recursively `shallowCopy`-ed, and stream bytes are copied only by
    /// sharing an existing buffer or registering a deferred provider.
    ///
    /// The source must have an owning document because qpdf's `newStream` is
    /// document-owned and `StreamCopier::copyStreamData` needs that document
    /// for the source-dispatch lifetime contract.
    pub fn copy_stream(&self) -> Result<ObjectHandle> {
        self.try_dereference()?;
        let Some(source_dict) = self.as_stream_dict() else {
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                self.type_name()
            )));
        };
        let resolver = self.context().ok_or_else(|| {
            Error::Internal("copyStream called on a stream with no owning PDF".to_owned())
        })?;
        let result = resolver.new_stream()?;
        let destination_dict = result.as_stream_dict().ok_or_else(|| {
            Error::Internal("copyStream created a non-stream destination".to_owned())
        })?;

        for (key, value) in source_dict.as_dictionary().unwrap_or_default() {
            let value = if value.is_indirect() {
                value
            } else {
                value.shallow_copy()?
            };
            destination_dict.replace_key(&key, value);
        }

        resolver.copy_stream_data(&result, self)?;
        Ok(result)
    }

    /// Merge `other`'s top-level entries into this handle's dictionary,
    /// mirroring `QPDFObjectHandle::mergeResources`
    /// (`libqpdf/QPDFObjectHandle.cc:1063-1153`; intended for merging two
    /// `/Resources`- or `/DR`-shaped dictionaries, per its own header doc,
    /// `include/qpdf/QPDFObjectHandle.hh:820-829`). `conflicts`, if given,
    /// records `rtype -> old_key -> new_key` for some (not all — see below)
    /// inner keys `other` had that collided with an existing key under the
    /// same top-level `rtype`.
    ///
    /// A no-op unless both `self` and `other` are dictionaries. For each of
    /// `other`'s top-level entries `(rtype, other_val)`:
    /// - if `self` has no `rtype` key yet, `other_val` is privatized via
    ///   [`Self::shallow_copy`] and installed via [`Self::replace_key`].
    /// - if `self`'s existing `rtype` value and `other_val` are both
    ///   dictionaries: `self`'s value is privatized first if it is
    ///   indirect (`shallow_copy` + `replace_key`, mirroring
    ///   `replaceKeyAndGetNew`'s combined mutate-and-rebind). Then each of
    ///   `other_val`'s own entries is merged in: a key the (now-private)
    ///   sub-dictionary does not have yet is installed directly
    ///   (privatized first unless already indirect); a key it already has
    ///   is left untouched unless `conflicts` is given, in which case an
    ///   incoming *indirect* value whose object identity already exists
    ///   somewhere in the sub-dictionary (as of the first such conflict
    ///   this call encounters — a snapshot taken once per `rtype`, not
    ///   re-taken per key) is reused under its existing name (`conflicts`
    ///   records this rename only when that existing name differs from the
    ///   incoming key — no rename is recorded, and nothing is installed,
    ///   when they already match); anything else is installed verbatim
    ///   under a freshly minted unique name (`conflicts` always records
    ///   this one).
    /// - if `self`'s existing `rtype` value and `other_val` are both
    ///   arrays: every scalar item in `other_val` whose
    ///   [`Self::unparse`] text does not already match a scalar item
    ///   already in `self`'s array is appended to it — a set union by
    ///   unparsed text, not object identity.
    /// - any other existing-`rtype` shape combination (mismatched types,
    ///   or neither dictionary nor array) leaves that entry untouched.
    ///
    /// The uniqueness pool for a freshly minted name is
    /// `this_val.getResourceNames()`'s own "second-level keys" definition
    /// (`libqpdf/QPDFObjectHandle.cc:1156-1170`) applied to the *inner*
    /// sub-dictionary itself, not to `self` as a whole — i.e. the keys of
    /// whichever of the sub-dictionary's *own* values are themselves
    /// dictionaries, not the sub-dictionary's own key set. This looks like
    /// it checks the wrong level (it does not, in general, collect the
    /// F1/F2-style names actually in scope), but it is qpdf's real,
    /// verified behavior, not a paraphrase — port it exactly rather than
    /// the more "sensible"-looking alternative of the sub-dictionary's own
    /// keys.
    ///
    /// See [`Self::replace_key`]'s doc comment for the same
    /// `resolve`/`resolve_borrowed` staleness caveat and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too,
    /// since this method installs and rebinds entries via `replace_key`.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::shallow_copy`]'s stream rejection: qpdf privatizes
    /// an incoming value with `shallowCopy` (`libqpdf/QPDFObjectHandle.cc:1090`,
    /// `:1113`, `:1122`), which throws for a *direct* stream, so a resources
    /// dictionary holding one — rather than the indirect reference a stream
    /// must be in a valid PDF — fails here too. Entries merged before the
    /// failing one stay installed, matching an exception unwinding out of
    /// qpdf's own loop.
    pub fn merge_resources(
        &self,
        other: &ObjectHandle,
        mut conflicts: Option<&mut ResourceConflicts>,
    ) -> Result<()> {
        let (Some(_), Some(other_entries)) = (self.as_dictionary(), other.as_dictionary()) else {
            return Ok(());
        };
        for (rtype, other_val) in other_entries {
            if !self.has_key(&rtype) {
                self.replace_key(&rtype, other_val.shallow_copy()?);
                continue;
            }
            let mut this_val = self.get_key(&rtype);
            if this_val.as_dictionary().is_some() && other_val.as_dictionary().is_some() {
                if this_val.is_indirect() {
                    let privatized = this_val.shallow_copy()?;
                    self.replace_key(&rtype, privatized.clone());
                    this_val = privatized;
                }
                merge_resource_subdict(&this_val, &other_val, &rtype, conflicts.as_deref_mut())?;
            } else if this_val.as_array().is_some() && other_val.as_array().is_some() {
                merge_resource_array(&this_val, &other_val);
            }
            // Any other shape combination for an existing rtype: untouched,
            // matching qpdf's own fallthrough (neither the dictionary nor
            // the array arm matches, and there is no further branch).
        }
        Ok(())
    }

    /// Replace this handle's stream data with the given buffer, and — when
    /// given — its `/Filter` and `/DecodeParms` dictionary keys, mirroring
    /// `QPDFObjectHandle::replaceStreamData`'s buffer overload
    /// (`libqpdf/QPDFObjectHandle.cc:1345-1350`, delegating to
    /// `QPDF_Stream::replaceStreamData`/`replaceFilterData`,
    /// `libqpdf/QPDF_Stream.cc:637-649,669-685`). `filter`/`decode_parms`
    /// are `Some` exactly where qpdf's own overload checks
    /// `QPDFObjectHandle::isInitialized()`: `Some` installs the key via
    /// [`Self::replace_key`], `None` leaves it untouched rather than
    /// removing it. A zero byte length removes `/Length`; a nonzero length
    /// installs the exact integer, matching qpdf's shared
    /// `QPDF_Stream::replaceFilterData` boundary for buffer and provider
    /// replacement. A no-op if this handle's value is not a stream.
    ///
    /// `data` is installed as given, not copied — qpdf's own
    /// `std::shared_ptr<Buffer>` overload is documented against its
    /// string overload precisely on that point
    /// (`include/qpdf/QPDFObjectHandle.hh:1086-1097`). This is what lets one
    /// buffer back two streams, as `QPDF::copyStreamData` does
    /// (`libqpdf/QPDF.cc:2240,2256-2258`).
    ///
    /// See [`Self::replace_key`]'s doc comment for the same
    /// `resolve`/`resolve_borrowed` staleness caveat and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too,
    /// since this method installs `/Filter`/`/DecodeParms`/`/Length` via
    /// `replace_key` and mutates the stream data in place.
    pub fn replace_stream_data(
        &self,
        data: Rc<Vec<u8>>,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) {
        let length = data.len();
        self.with_value_mut(|v| {
            if let Some(ObjectValue::Stream {
                stream_data: existing,
                stream_provider,
                ..
            }) = v
            {
                *existing = Some(data);
                *stream_provider = None;
            }
        });
        self.replace_filter_data(filter, decode_parms, length);
    }

    /// Replace this handle's stream source with a deferred qpdf-style
    /// [`StreamDataProvider`]. The provider is retained without being called
    /// or materialized. The provider source clears any replaced buffer, and a
    /// zero length is passed to the shared filter boundary so `/Length` is
    /// removed until the pipe path observes the provider's actual output.
    ///
    /// `None` filter/decode parameters preserve existing keys. An explicit
    /// [`ObjectHandle::null`] removes them through the canonical dictionary
    /// mutation path. A non-stream handle returns qpdf's `asStreamWithAssert`
    /// runtime classification as [`Error::System`].
    pub fn replace_stream_data_provider(
        &self,
        provider: Rc<dyn StreamDataProvider>,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) -> Result<()> {
        self.try_dereference()?;
        if !self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. }))) {
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                self.type_name()
            )));
        }

        self.with_value_mut(|value| {
            if let Some(ObjectValue::Stream {
                stream_data,
                stream_provider: existing,
                ..
            }) = value
            {
                *stream_data = None;
                *existing = Some(provider);
            }
        });
        self.replace_filter_data(filter, decode_parms, 0);
        Ok(())
    }

    /// Register a qpdf-style void callback as a deferred stream provider.
    pub fn replace_stream_data_with_callback<F>(
        &self,
        callback: F,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) -> Result<()>
    where
        F: Fn(&mut dyn Pipeline) -> Result<()> + 'static,
    {
        self.replace_stream_data_provider(
            Rc::new(CallbackProvider { callback }),
            filter,
            decode_parms,
        )
    }

    /// Register a qpdf-style retry-aware callback as a deferred stream
    /// provider. Its return value is consumed by the provider pipe boundary.
    pub fn replace_stream_data_with_retry_callback<F>(
        &self,
        callback: F,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) -> Result<()>
    where
        F: Fn(&mut dyn Pipeline, bool, bool) -> Result<bool> + 'static,
    {
        self.replace_stream_data_provider(
            Rc::new(RetryCallbackProvider { callback }),
            filter,
            decode_parms,
        )
    }

    /// Apply the filter and length dictionary mutations shared by qpdf's
    /// buffer and provider `QPDF_Stream::replaceStreamData` overloads
    /// (`libqpdf/QPDF_Stream.cc:640-684`).
    fn replace_filter_data(
        &self,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
        length: usize,
    ) {
        let Some(dict) = self.as_stream_dict() else {
            return;
        };
        if let Some(filter) = filter {
            dict.replace_key(b"/Filter", filter);
        }
        if let Some(decode_parms) = decode_parms {
            dict.replace_key(b"/DecodeParms", decode_parms);
        }
        if length == 0 {
            dict.remove_key(b"/Length");
        } else {
            dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(length).unwrap_or(i64::MAX)),
            );
        }
    }

    /// The stream's own dictionary handle if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is a stream,
    /// or `None` otherwise. This never performs resolution itself: an
    /// indirect handle that has not yet been resolved returns `None` too,
    /// the same as a resolved value of a different type. Cloning the
    /// returned handle is O(1): it shares the dictionary's identity rather
    /// than copying its subtree.
    pub fn as_stream_dict(&self) -> Option<ObjectHandle> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { stream_dict, .. }) => Some(stream_dict.clone()),
            _ => None,
        })
    }

    /// The stream's raw encoded byte payload if this handle's value — its
    /// own if direct, or its already-resolved value if indirect — is a
    /// stream, or `None` otherwise. This never performs resolution itself:
    /// an indirect handle that has not yet been resolved returns `None`
    /// too, the same as a resolved value of a different type.
    ///
    /// The payload is shared, not copied: this hands out the same allocation
    /// the stream holds, mirroring `QPDF_Stream::getStreamDataBuffer`
    /// (`libqpdf/qpdf/QPDF_Stream.hh:39`), which returns qpdf's
    /// `std::shared_ptr<Buffer>` itself.
    pub fn as_stream_data(&self) -> Option<Rc<Vec<u8>>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { stream_data, .. }) => stream_data.clone(),
            _ => None,
        })
    }

    /// Pipe this stream through qpdf's filter branch.
    ///
    /// This is the `QPDFObjectHandle::pipeStreamData` entry point
    /// (`libqpdf/QPDFObjectHandle.cc:1300-1341`) over the
    /// `QPDF_Stream::filterable` and reverse-stage construction owned here
    /// (`libqpdf/QPDF_Stream.cc:379-569`). `filtering_attempted` is the
    /// qpdf out-parameter: it records a usable installed filter branch and is
    /// cleared if the source branch fails; the returned bool is overall
    /// source-pipeline success. Keeping those results separate is what lets a
    /// writer retry a failed filtering decision with raw bytes
    /// (`libqpdf/QPDFWriter.cc:1239-1314`).
    ///
    /// `encode_flags` uses [`STREAM_ENCODE_COMPRESS`] and
    /// [`STREAM_ENCODE_NORMALIZE`]. The output stages are built first, then
    /// the stream filters are added in reverse `/Filter` order. The source
    /// is finally dispatched through the completed chain without a legacy
    /// `Object` materialization.
    #[allow(dead_code)] // writer/inspection consumers are not on the canonical resolver route yet.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pipe_stream_data(
        &self,
        pipeline: &mut dyn Pipeline,
        filtering_attempted: &mut bool,
        encode_flags: u32,
        decode_level: DecodeLevel,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        self.pipe_stream_data_inner(
            pipeline,
            filtering_attempted,
            encode_flags,
            decode_level,
            suppress_warnings,
            will_retry,
            false,
        )
    }

    /// The object-stream resolver uses qpdf's resolve-time catch boundary for
    /// codec failures raised while decoding replaced stream data. Ordinary
    /// callers keep the original pipeline error so writer and inspection
    /// paths do not silently turn their own sink failures into recovery.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pipe_stream_data_for_object_stream(
        &self,
        pipeline: &mut dyn Pipeline,
        filtering_attempted: &mut bool,
        encode_flags: u32,
        decode_level: DecodeLevel,
        suppress_warnings: bool,
        will_retry: bool,
    ) -> Result<bool> {
        self.pipe_stream_data_inner(
            pipeline,
            filtering_attempted,
            encode_flags,
            decode_level,
            suppress_warnings,
            will_retry,
            true,
        )
    }

    /// Return decoded stream data through the canonical source pipeline.
    ///
    /// This is qpdf's `QPDFObjectHandle::getStreamData`
    /// (`libqpdf/QPDFObjectHandle.cc:1289-1292`) over the same
    /// `QPDF_Stream::pipeStreamData` path used by page-content piping
    /// (`libqpdf/QPDFObjectHandle.cc:1710-1722`). Unlike
    /// [`Self::get_raw_stream_data`], this path decrypts document-backed
    /// streams before applying their filters, so recovered stream framing is
    /// handled at the source boundary rather than being exposed as decoded
    /// page content.
    pub fn get_stream_data(&self, decode_level: DecodeLevel) -> Result<Rc<Vec<u8>>> {
        let mut buffer = crate::pipeline::buffer::Buffer::new("stream data", None);
        let mut filtering_attempted = false;
        let stream_data_succeeded = self.pipe_stream_data(
            &mut buffer,
            &mut filtering_attempted,
            0,
            decode_level,
            false,
            false,
        )?; // cov:ignore: multiline call terminator has no executable coverage region
        if !stream_data_succeeded {
            return Err(Error::Unsupported(
                "error getting decoded stream data".to_owned(),
            ));
        }
        Ok(Rc::new(buffer.take_buffer()?))
    }

    #[allow(clippy::too_many_arguments)]
    fn pipe_stream_data_inner(
        &self,
        pipeline: &mut dyn Pipeline,
        filtering_attempted: &mut bool,
        encode_flags: u32,
        decode_level: DecodeLevel,
        suppress_warnings: bool,
        will_retry: bool,
        recover_codec_errors: bool,
    ) -> Result<bool> {
        self.try_dereference()?;
        let Some((stream_dict, stream_data, stream_provider, stream_length)) =
            self.with_value(|value| match value {
                Some(ObjectValue::Stream {
                    stream_dict,
                    stream_data,
                    stream_provider,
                    stream_length,
                    ..
                }) => Some((
                    stream_dict.clone(),
                    stream_data.clone(),
                    stream_provider.clone(),
                    *stream_length,
                )),
                _ => None,
            })
        else {
            return Err(Error::Internal(
                "pipeStreamData called for non-stream".to_owned(),
            ));
        };

        *filtering_attempted = false;
        let filter_requested = encode_flags != 0 || !matches!(decode_level, DecodeLevel::None);
        if !filter_requested {
            return self.pipe_stream_source(
                &stream_dict,
                stream_data,
                stream_provider.clone(),
                stream_length,
                pipeline,
                suppress_warnings,
                will_retry,
                false,
            );
        }

        let Some(mut plan) = self.prepare_stream_filter_plan(&stream_dict)? else {
            return self.pipe_stream_source(
                &stream_dict,
                stream_data,
                stream_provider.clone(),
                stream_length,
                pipeline,
                suppress_warnings,
                will_retry,
                false,
            );
        };
        if (plan.lossy_compression && decode_level < DecodeLevel::All)
            || (plan.specialized_compression && decode_level < DecodeLevel::Specialized)
        {
            return self.pipe_stream_source(
                &stream_dict,
                stream_data,
                stream_provider.clone(),
                stream_length,
                pipeline,
                suppress_warnings,
                will_retry,
                false,
            );
        }

        *filtering_attempted = true;
        let mut head = PipelineRef::Borrowed(pipeline);
        let warning_delivery_error: Rc<RefCell<Option<Error>>> = Rc::new(RefCell::new(None));
        let normalization_warnings =
            if encode_flags & STREAM_ENCODE_NORMALIZE != 0 && !suppress_warnings {
                Some(Rc::new(RefCell::new(Vec::new())))
            } else {
                None
            };
        if encode_flags & STREAM_ENCODE_COMPRESS != 0 {
            let compress = Flate::new(
                "compress stream",
                head,
                FlateAction::Deflate,
                DEFAULT_OUT_BUFFER_SIZE,
            )?; // cov:ignore: fixed nonzero output buffer makes Flate::new's failure branch untestable here
            head = PipelineRef::Owned(Box::new(compress));
        }
        if encode_flags & STREAM_ENCODE_NORMALIZE != 0 {
            let mut normalizer = ContentNormalizerPipeline::new("normalizer", head);
            if let Some(warnings) = normalization_warnings.as_ref() {
                let warnings = Rc::clone(warnings);
                normalizer.set_warning_callback(Box::new(move |message| {
                    warnings.borrow_mut().push(message.to_owned());
                    Ok(())
                }));
            }
            head = PipelineRef::Owned(Box::new(normalizer));
        }

        for filter in plan.filters.iter_mut().rev() {
            let warning_handle = self.clone();
            let warning_delivery_error = Rc::clone(&warning_delivery_error);
            filter.set_warning_callback(Box::new(move |message, _code| {
                match warning_handle.object_warning(message) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        let error_message = error.to_string();
                        *warning_delivery_error.borrow_mut() = Some(error);
                        Err(PipelineError::runtime(error_message))
                    }
                }
            }));
            head = match filter.decode_pipeline_owned(head)? {
                OwnedDecodePipeline::Stage(stage) => PipelineRef::Owned(stage),
                OwnedDecodePipeline::NoStage(next) => next,
            };
        }

        let success = self
            .pipe_stream_source(
                &stream_dict,
                stream_data,
                stream_provider,
                stream_length,
                &mut head,
                suppress_warnings,
                will_retry,
                recover_codec_errors,
            )
            .map_err(|error| warning_delivery_error.borrow_mut().take().unwrap_or(error))?;
        if let Some(error) = warning_delivery_error.borrow_mut().take() {
            // A source-backed decoder can recover its codec failure and
            // return `false` after the warning callback itself failed. Keep
            // that callback error visible to the resolver instead of
            // converting the member to qpdf's ordinary null fallback.
            return Err(error);
        }
        if !success {
            *filtering_attempted = false;
        }
        if success {
            if let Some(warnings) = normalization_warnings {
                for warning in warnings.borrow_mut().drain(..) {
                    self.object_warning(&warning)?;
                }
            }
        }
        Ok(success)
    }

    fn prepare_stream_filter_plan(
        &self,
        stream_dict: &ObjectHandle,
    ) -> Result<Option<StreamFilterPlan>> {
        let filter = stream_dict.try_get_key(b"/Filter")?;
        let filter_names = if filter.try_is_null()? {
            Vec::new()
        } else if let Some(name) = filter.try_as_name()? {
            vec![name]
        } else if let Some(count) = filter.try_array_len()? {
            let mut names = Vec::with_capacity(count);
            let mut malformed = false;
            for index in 0..count {
                let item = filter.try_array_item(index)?.ok_or_else(|| {
                    // cov:ignore-start: immutable array length and item lookup cannot diverge
                    Error::Internal("filter array item disappeared during inspection".to_owned())
                    // cov:ignore-end
                    // cov:ignore-start: LLVM attributes the unreachable defensive closure result here
                })?;
                // cov:ignore-end
                if let Some(name) = item.try_as_name()? {
                    names.push(name);
                } else {
                    malformed = true;
                }
            }
            if malformed {
                self.object_warning(FILTER_TYPE_ERROR)?;
                return Ok(None);
            }
            names
        } else {
            self.object_warning(FILTER_TYPE_ERROR)?;
            return Ok(None);
        };

        // qpdf ignores /DecodeParms entirely when /Filter is empty.
        if filter_names.is_empty() {
            return Ok(Some(StreamFilterPlan {
                filters: Vec::new(),
                specialized_compression: false,
                lossy_compression: false,
            }));
        }

        // qpdf looks up every factory before it reads /DecodeParms. Keeping
        // this ordering is observable for an unknown filter paired with a
        // dangling or mismatched parameter object.
        let mut filters = Vec::with_capacity(filter_names.len());
        for name in &filter_names {
            let normalized_name = normalize_filter_name(name);
            let Some(filter) = stream_filter_for(normalized_name) else {
                return Ok(None);
            };
            filters.push(filter);
        }

        let decode_params = stream_dict.try_get_key(b"/DecodeParms")?;
        let decode_param_handles = if decode_params.try_is_null()? {
            vec![ObjectHandle::null(); filter_names.len()]
        } else if let Some(count) = decode_params.try_array_len()? {
            if count == 0 {
                vec![ObjectHandle::null(); filter_names.len()]
            } else {
                if count != filter_names.len() {
                    self.object_warning(DECODE_PARMS_LENGTH_ERROR)?;
                    return Ok(None);
                }
                let mut handles = Vec::with_capacity(count);
                for index in 0..count {
                    // cov:ignore-start: llvm-cov attributes this defensive closure to the call line
                    handles.push(decode_params.try_array_item(index)?.ok_or_else(|| {
                        Error::Internal(
                            "decode parameters array item disappeared during inspection".to_owned(),
                        )
                    })?);
                    // cov:ignore-end
                }
                handles
            }
        } else {
            vec![decode_params; filter_names.len()]
        };

        let mut plan = StreamFilterPlan {
            filters: Vec::with_capacity(filter_names.len()),
            specialized_compression: false,
            lossy_compression: false,
        };
        for ((name, mut filter), decode_params) in filter_names
            .into_iter()
            .zip(filters)
            .zip(decode_param_handles)
        {
            let filter_name = normalize_filter_name(&name);
            let decode_params = decode_params_from_handle(&decode_params, filter_name)?;
            if !filter.set_decode_params(&decode_params) {
                return Ok(None);
            }
            plan.specialized_compression |= filter.is_specialized_compression();
            plan.lossy_compression |= filter.is_lossy_compression();
            plan.filters.push(filter);
        }
        Ok(Some(plan))
    }

    /// qpdf `QPDF_Stream::getRawStreamData` (`libqpdf/QPDF_Stream.cc:362-376`).
    ///
    /// Replaced stream data is written directly; original data is read through
    /// the owning document at the parsed offset and stored parse-time length.
    /// No filter or decoder stage is constructed here.
    pub fn get_raw_stream_data(&self) -> Result<Rc<Vec<u8>>> {
        let mut buffer = crate::pipeline::buffer::Buffer::new("stream data", None);
        if !self.pipe_raw_stream_data(&mut buffer)? {
            return Err(Error::Unsupported(
                "error getting raw stream data".to_owned(),
            ));
        }
        Ok(Rc::new(buffer.take_buffer()?))
    }

    fn pipe_raw_stream_data(&self, pipeline: &mut dyn crate::pipeline::Pipeline) -> Result<bool> {
        self.try_dereference()?;
        let Some((stream_dict, stream_data, stream_provider, stream_length)) =
            self.with_value(|value| match value {
                Some(ObjectValue::Stream {
                    stream_dict,
                    stream_data,
                    stream_provider,
                    stream_length,
                    ..
                }) => Some((
                    stream_dict.clone(),
                    stream_data.clone(),
                    stream_provider.clone(),
                    *stream_length,
                )),
                _ => None,
            })
        else {
            return Err(Error::Internal(
                "pipeStreamData called for non-stream".to_owned(),
            ));
        };

        self.pipe_stream_source(
            &stream_dict,
            stream_data,
            stream_provider,
            stream_length,
            pipeline,
            false,
            false,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn pipe_stream_source(
        &self,
        stream_dict: &ObjectHandle,
        stream_data: Option<Rc<Vec<u8>>>,
        stream_provider: Option<Rc<dyn StreamDataProvider>>,
        stream_length: usize,
        pipeline: &mut dyn Pipeline,
        suppress_warnings: bool,
        will_retry: bool,
        recover_codec_errors: bool,
    ) -> Result<bool> {
        if let Some(stream_data) = stream_data {
            pipeline
                .write(&stream_data)
                .map_err(|error| Self::map_stream_pipeline_error(error, recover_codec_errors))?;
            pipeline
                .finish()
                .map_err(|error| Self::map_stream_pipeline_error(error, recover_codec_errors))?;
            return Ok(true);
        }

        // qpdf dispatches the deferred source after replaced bytes and before
        // the parsed-offset/original branch (`libqpdf/QPDF_Stream.cc:571-620`).
        // Pl_Count is part of that provider boundary: it forwards every
        // incremental write and measures the bytes that actually reached the
        // decoder/output pipeline (`libqpdf/QPDF_Stream.cc:575-604`).
        if let Some(provider) = stream_provider {
            let object_ref = self.object_ref().ok_or_else(|| {
                Error::Internal(
                    "pipeStreamData called for provider-backed direct stream".to_owned(),
                )
            })?;
            let mut count = Count::new("stream provider count", pipeline);
            let success = if provider.supports_retry() {
                provider.provide_stream_data_with_retry(
                    object_ref,
                    &mut count,
                    suppress_warnings,
                    will_retry,
                )?
            } else {
                provider.provide_stream_data(object_ref, &mut count)?;
                true
            };
            if !success {
                return Ok(false);
            }

            let actual_length = i64::try_from(count.count()).map_err(|_| {
                // cov:ignore-start: an in-memory provider cannot emit more than i64::MAX bytes in a test
                Error::System("stream data provider length exceeds PDF integer range".to_owned())
                // cov:ignore-end
            })?; // cov:ignore: closing line of the unreachable signed-PDF-range guard
            if stream_dict.try_has_key(b"/Length")? {
                let desired_length = stream_dict.try_get_key(b"/Length")?.try_get_int_value()?;
                if actual_length != desired_length {
                    return Err(Error::System(format!(
                        "stream data provider for {} {} provided {} bytes instead of expected {} bytes",
                        object_ref.number, object_ref.generation, actual_length, desired_length
                    )));
                }
            } else {
                stream_dict.replace_key(b"/Length", ObjectHandle::integer(actual_length));
            }
            return Ok(true);
        }

        let parsed_offset = self.get_parsed_offset();
        if parsed_offset == 0 {
            return Err(Error::Internal(
                "pipeStreamData called for stream with no data".to_owned(),
            ));
        }

        let (object_ref, resolver) = {
            let slot = self.0.borrow();
            let Some(object_ref) = slot.object_ref else {
                return Err(Error::Internal(
                    "pipeStreamData called for original direct stream".to_owned(),
                ));
            };
            (object_ref, slot.resolver.clone())
        };
        let Some(resolver) = resolver.and_then(|resolver| resolver.upgrade()) else {
            return Err(Error::Internal(format!(
                "object {} {} belongs to a dropped PDF",
                object_ref.number, object_ref.generation
            )));
        };
        resolver.pipe_stream_data(
            object_ref,
            parsed_offset,
            stream_length,
            stream_dict,
            pipeline,
            suppress_warnings,
            will_retry,
        )
    }

    fn map_stream_pipeline_error(error: PipelineError, recover_codec_errors: bool) -> Error {
        if recover_codec_errors {
            if let PipelineError::Runtime(message) = error {
                return Error::Unsupported(format!(
                    "error decoding stream data: {}",
                    message.into_string_lossy()
                ));
            }
        }
        error.into()
    }

    /// The value as raw operator bytes if this handle's value — its own if
    /// direct, or its already-resolved value if indirect — is a
    /// content-stream operator token, or `None` otherwise. Never performs
    /// resolution itself.
    pub fn as_operator(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Operator(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The value as raw inline-image bytes if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is an
    /// inline-image payload, or `None` otherwise. Never performs resolution
    /// itself.
    pub fn as_inline_image(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::InlineImage(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The qpdf-compatible numeric type code of this handle's current known
    /// value: `include/qpdf/Constants.h:108-127`'s `qpdf_object_type_e`
    /// ordinals. qpdf's own `getTypeCode()`/`getTypeName()`
    /// (`include/qpdf/QPDFObjectHandle.hh:311-316`,
    /// `libqpdf/QPDFObjectHandle.cc:240-250`) call `dereference()`, which
    /// unconditionally resolves the handle first
    /// (`libqpdf/QPDFObjectHandle.cc:2376-2382`); this method never performs
    /// that hidden resolution (design, `Pdf` section: no hidden I/O), so an
    /// indirect handle's *reachable* resolution states surface as their own
    /// qpdf ordinals instead: not-yet-resolved reports `13`
    /// (`ot_unresolved`) and a destroyed (owning document dropped) handle
    /// reports `14` (`ot_destroyed`) — both real `qpdf_object_type_e`
    /// entries, not invented here. A reserved handle (see [`crate::Pdf::new_reserved`])
    /// reports `1` (`ot_reserved`), qpdf's own ordinal for that state
    /// (`include/qpdf/Constants.h:108-127`). `ot_uninitialized` (qpdf's one
    /// remaining entry) is a construction-time-only state this port's
    /// `ObjectHandle` never occupies, since every non-reserved handle is
    /// fully constructed at birth.
    ///
    /// A resolved indirect handle whose own value is itself a bare
    /// reference (mirroring [`crate::Object::Reference`]; see
    /// [`Self::as_reference`]'s own doc), a `Pdf::set_object`-driven
    /// redirect, also reports `13`. This looks like a
    /// contradiction with [`Self::is_resolved`] returning `true` for the
    /// same handle, but it is not: the *value* is known (it is a reference),
    /// while the *referenced object's own type* is not known without
    /// following the chain further, which this method never does — this
    /// case is not chased to its terminal type the way it would be
    /// elsewhere in this crate's own object-inspection code, and `13`
    /// (`ot_unresolved`) is reported as a placeholder rather than the
    /// terminal object's real ordinal.
    pub fn type_code(&self) -> u8 {
        {
            // `Destroyed` is a value state, independent of the indirect
            // metadata that disconnect clears. Bind the borrow to a local
            // and release it before `with_value` below takes its own borrow.
            let slot_ref = self.0.borrow();
            let state = slot_ref.state.borrow();
            match &*state {
                ObjectState::Reserved => return 1,
                ObjectState::Destroyed => return 14,
                ObjectState::NotYetResolved if slot_ref.object_ref.is_some() => return 13,
                ObjectState::NotYetResolved | ObjectState::Missing | ObjectState::Resolved(_) => {}
            }
        }
        self.with_value(|value| {
            match value.expect(
                "every reachable state here (direct, indirect Missing, indirect Resolved) carries a value",
            ) {
                ObjectValue::Null => 2,
                ObjectValue::Boolean(_) => 3,
                ObjectValue::Integer(_) => 4,
                ObjectValue::Real(_) | ObjectValue::RealLiteral { .. } => 5,
                ObjectValue::String(_) => 6,
                ObjectValue::Name(_) => 7,
                ObjectValue::Array(_) => 8,
                ObjectValue::Dictionary(_) => 9,
                ObjectValue::Stream { .. } => 10,
                ObjectValue::Operator(_) => 11,
                ObjectValue::InlineImage(_) => 12,
                // See this method's own doc for why this maps to
                // `ot_unresolved`: a real, reachable state (via
                // `Pdf::set_object`), not speculative dead code — see
                // `resolved_to_a_reference_indirect_handle_reports_unresolved`
                // for a test that exercises it via the same `set_resolved`
                // call `Pdf::set_object` itself makes.
                ObjectValue::Reference(_) => 13,
            }
        })
    }

    /// The qpdf-compatible type name string for [`Self::type_code`]'s
    /// ordinal (`libqpdf/QPDFObjectHandle.cc:240-250`'s `getTypeName`, via
    /// each `QPDFValue` subclass's own registered name, e.g.
    /// `libqpdf/QPDF_InlineImage.cc:6`). See [`Self::type_code`]'s own doc
    /// for the states this port surfaces instead of qpdf's silent resolve.
    pub fn type_name(&self) -> &'static str {
        match self.type_code() {
            1 => "reserved",
            2 => "null",
            3 => "boolean",
            4 => "integer",
            5 => "real",
            6 => "string",
            7 => "name",
            8 => "array",
            9 => "dictionary",
            10 => "stream",
            11 => "operator",
            12 => "inline-image",
            14 => "destroyed",
            // `type_code` only ever returns 13 for any other value it can
            // produce, so this is exhaustive in practice, not a silent
            // catch-all for an unhandled ordinal.
            _ => "unresolved",
        }
    }

    // `None` for an indirect handle that has not yet been resolved — value
    // access on an unresolved handle must not perform hidden I/O (design,
    // `Pdf` section). A resolved indirect handle exposes its real value.
    // Missing and Destroyed retain the null fallback for value accessors that
    // use this helper; state-aware accessors such as `is_null` inspect their
    // slot directly instead.
    fn with_value<T>(&self, f: impl FnOnce(Option<&ObjectValue>) -> T) -> T {
        let state = self.0.borrow().state.clone();
        let state = state.borrow();
        match &*state {
            ObjectState::NotYetResolved => f(None),
            ObjectState::Resolved(value) => f(Some(value)),
            ObjectState::Missing | ObjectState::Destroyed => f(Some(&ObjectValue::Null)),
            ObjectState::Reserved => f(None),
        }
    }

    // Mutable twin of `with_value` above: `None` for an indirect handle not
    // yet resolved (mutation on an unresolved handle must not perform
    // hidden I/O, same rule as every read accessor), and for
    // `Missing`/`Destroyed` (there is no live `ObjectValue::Null` slot to
    // hand out a `&mut` into — those states only *present* as null, they do
    // not store one).
    fn with_value_mut<T>(&self, f: impl FnOnce(Option<&mut ObjectValue>) -> T) -> T {
        let state = self.0.borrow().state.clone();
        let mut state = state.borrow_mut();
        match &mut *state {
            ObjectState::Resolved(value) => f(Some(value)),
            ObjectState::NotYetResolved
            | ObjectState::Missing
            | ObjectState::Reserved
            | ObjectState::Destroyed => f(None),
        }
    }

    /// Convert this handle's value into a legacy [`crate::Object`] tree —
    /// `Pdf::resolve`/`Pdf::resolve_borrowed`'s own materialization bridge,
    /// also public for a caller outside this crate that still needs a
    /// legacy `Object`/[`Dictionary`] for one value reached through an
    /// otherwise `ObjectHandle`-native walk (e.g. `flpdf-qtest-tools`' qtest
    /// driver, which ports a `&Dictionary`-shaped qpdf filter/`DecodeParms`
    /// resolution routine).
    ///
    /// An indirect array/dictionary child is *not* recursively resolved: it
    /// becomes `Object::Reference(child_ref)`, matching the parser's
    /// pre-existing `Object::Reference` semantics so every consumer match on
    /// that variant keeps working unchanged. A stream's own dictionary
    /// handle (a separately parsed handle with its own `<<`-start parsed
    /// offset) is flattened into a plain [`Dictionary`] for
    /// `Object::Stream`.
    ///
    /// Parsed original streams make this bridge fallible: materializing one
    /// reads its bytes through [`Self::get_raw_stream_data`] at this call
    /// site. New qpdf-native stream consumers must keep the handle and pipe
    /// it instead; this legacy bridge exists only for callers that still
    /// require an owned [`Object`].
    ///
    /// An indirect handle that has not yet been resolved (see
    /// [`Self::is_resolved`]) materializes as `Object::Null` rather than
    /// performing hidden resolution; callers that need the real value must
    /// resolve first (e.g. via `Pdf::resolve_object_handle`).
    ///
    /// A tree built through the public [`Self::array`]/[`Self::dictionary`]
    /// factories carries no depth bound the way parsed input does, so this
    /// walk is wrapped with the same stack-growth protection
    /// [`Self::unparse_resolved`]/[`Self::shallow_copy`] already rely on
    /// during construction, *and* capped at `parser::MAX_PARSE_DEPTH`,
    /// substituting `Object::Null` for anything nested past that — no
    /// document this crate accepts could parse a value nested deeper, so
    /// the cap only ever bites a tree built directly through those
    /// factories. Growing the construction stack alone would not be
    /// enough on its own: the *returned* `Object` tree's own ordinary
    /// recursive `Drop` runs later, unprotected, once this method has
    /// already returned and the grown stack is gone — the depth cap keeps
    /// that later drop within a size every other `MAX_PARSE_DEPTH`-bounded
    /// `Object` tree in this crate already handles routinely, rather than
    /// trying to protect `Drop` itself (`Object`'s recursive `Drop` glue
    /// lives in `object.rs`, outside this file's scope to change).
    ///
    /// This does not protect `self` — the handle passed in — the same way:
    /// an `ObjectHandle` tree that deep, built through the same public
    /// factories, is a separate, pre-existing gap in `ObjectHandle`'s own
    /// `Drop`, reachable without ever calling `materialize` at all (it
    /// existed as long as those factories have been public), not something
    /// introduced or fixable here.
    pub fn materialize(&self) -> Result<Object> {
        if self.is_reserved() {
            return Err(Error::System(
                "QPDFObjectHandle: attempting to unparse a reserved object".to_owned(),
            ));
        }
        materialize_bounded(self, 0)
    }

    /// This handle's qpdf-syntax unparse form
    /// (`include/qpdf/QPDFObjectHandle.hh:1159`,
    /// `libqpdf/QPDFObjectHandle.cc:1574-1584`): an indirect handle always
    /// unparses to its own `"N G R"`, regardless of resolution state; a
    /// direct handle delegates to [`Self::unparse_resolved`].
    pub fn unparse(&self) -> Vec<u8> {
        match self.object_ref() {
            Some(object_ref) => {
                let mut out = Vec::new();
                Object::Reference(object_ref).write_pdf(&mut out);
                out
            }
            None => self.unparse_resolved(),
        }
    }

    /// This handle's resolved value in qpdf syntax
    /// (`libqpdf/QPDFObjectHandle.cc:1586-1593`), except that an *indirect*
    /// handle whose resolved value is a stream always reports its own
    /// reference form instead (`libqpdf/QPDF_Stream.cc:173-178`) — a stream
    /// is only ever a top-level indirect object in valid qpdf usage. A
    /// direct handle wrapping a stream value (a shape this crate's own
    /// types do not forbid, though qpdf's do) falls through to the same
    /// inlining fallback as any other direct container value; see
    /// `unparse_tests`' own direct-stream test for that case.
    ///
    /// This port diverges from qpdf's own `unparseResolved()` in three
    /// internal resolution states that qpdf itself does not reach the same
    /// way:
    /// - **Not yet resolved**: qpdf silently dereferences (resolves) an
    ///   unresolved indirect handle before unparsing it; this method does
    ///   not perform that hidden I/O, matching every other accessor in this
    ///   file (see e.g. [`Self::as_integer`]'s own doc) — no accessor here
    ///   resolves on the caller's behalf. Reports the same `null` fallback
    ///   the value would show before resolution.
    /// - **Destroyed** (the owning document has been dropped and this
    ///   handle's value severed): qpdf's `QPDF_Destroyed::unparse()`
    ///   (`libqpdf/QPDF_Destroyed.cc:24-29`) throws `std::logic_error`; this
    ///   method has no exception channel to mirror that with (`Vec<u8>`
    ///   return, no `Result`) and instead retains its null fallback rather
    ///   than panicking. [`Self::is_null`] intentionally remains false for a
    ///   destroyed handle.
    /// - **Reserved** (see [`crate::Pdf::new_reserved`], not yet replaced with a
    ///   real value): qpdf's `QPDF_Reserved::unparse()`
    ///   (`libqpdf/QPDF_Reserved.cc:22-26`) throws `std::logic_error` the
    ///   same way `QPDF_Destroyed::unparse()` does, for the same reason —
    ///   this method has no exception channel to mirror that with and falls
    ///   back to `null` here too. Unlike Destroyed/Not-yet-resolved, the
    ///   writer-facing top-level entry points ([`Self::materialize`],
    ///   `unparse_object_walk` and its QDF/ref-map siblings,
    ///   `unparse_stream_body` and its siblings, `unparse_trailer`) do
    ///   reject a reserved handle with an error, since those return
    ///   `Result` — but only when the reserved handle *is* the value being
    ///   dereferenced there, mirroring where qpdf's own throw is actually
    ///   reached (`QPDFObjectHandle::unparseResolved`,
    ///   `QPDFObjectHandle.cc:1586-1592`, dereferencing before calling the
    ///   resolved value's own `unparse()`). A reserved handle reached only
    ///   as an indirect *child* of another container is never rejected:
    ///   `materialize_child`/`write_child` and their QDF/ref-map siblings
    ///   write its own `"N G R"` reference form like any other indirect
    ///   child, matching `QPDFWriter::unparseChild`
    ///   (`libqpdf/QPDFWriter.cc:1144-1156`), which checks only
    ///   `isIndirect()` and never inspects what the reference resolves to.
    ///   This method is the one place in this file that cannot follow
    ///   either suit.
    pub fn unparse_resolved(&self) -> Vec<u8> {
        // Bridges through a null-omission-aware materialization walk
        // (`unparse_materialize`, distinct from the general `materialize`/
        // `Pdf::resolve_borrowed` bridge -- see that function's own doc)
        // and `Object::write_pdf`'s own already-byte-identical-tested
        // formatter rather than duplicating array/dict/string-escaping
        // logic against `ObjectValue` directly.
        let is_stream = self.object_ref().is_some()
            && self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. })));
        if is_stream {
            return self.unparse();
        }
        let mut out = Vec::new();
        let materialized = unparse_materialize(self);
        materialized.write_pdf(&mut out);
        // `Object`'s own recursive Drop glue would walk this tree exactly
        // as deep as the walk above just did, unprotected by
        // `stacker::maybe_grow` -- protecting construction alone would
        // still let a deep enough tree crash the process immediately after
        // serialization completes, right here. Tear it down iteratively
        // instead of letting it drop normally.
        unparse_drop_iteratively(materialized);
        out
    }

    /// Replace this handle's own value in place, preserving its identity
    /// (every other outstanding clone observes the new value) and its
    /// already-recorded parsed offset (`parsed_offset` is untouched here --
    /// see [`Self::reset_parsed_offset`] to clear it). A no-op for an
    /// indirect handle; see [`Self::set_resolved`] for that case.
    ///
    /// Used by `Pdf::set_object` to update a stream's own dictionary handle
    /// in place when the replacement value is also a stream, so the
    /// dictionary handle's already-recorded `<<`-start parsed offset
    /// survives instead of being lost to a freshly minted handle.
    pub(crate) fn replace_direct_value(&self, value: ObjectValue) {
        if self.is_direct() {
            self.replace_shared_state(ObjectState::Resolved(canonicalize_object_value(value)));
        }
    }

    /// Reset this handle's parsed offset back to the no-offset sentinel,
    /// overriding the set-once contract [`Self::set_parsed_offset_if_unset`]
    /// normally enforces.
    ///
    /// Used by `Pdf::set_object`: once it replaces an indirect handle's
    /// value with a caller-supplied one, any previously recorded source
    /// position no longer describes that value.
    pub(crate) fn reset_parsed_offset(&self) {
        self.0.borrow_mut().parsed_offset = NO_PARSED_OFFSET;
    }

    /// This handle's plain (non-QDF) writer-emission form
    /// (`QPDFWriter::unparseObject`, `QPDFWriter.cc:1318-1527`, called with
    /// `level=0, flags=0`). Distinct from [`Self::unparse`]/
    /// [`Self::unparse_resolved`], which port a different qpdf function
    /// (`QPDFObjectHandle::unparse`) with a different contract — do not
    /// conflate the two. Forces resolution of `self` (mirroring qpdf's own
    /// implicit `dereference()` on `object`'s first `isXxx()` type check
    /// inside `unparseObject` itself) and of every indirect dictionary
    /// entry reached along the way, to apply qpdf's null-valued-key
    /// suppression rule (`:1490-1491`); an indirect entry that survives
    /// suppression writes as its own `"N G R"` reference form, never
    /// inlined.
    ///
    /// If `self` is an *indirect* handle whose resolved value is a `Stream`,
    /// this call reaches `unparse_object_value`'s `Stream` arm directly (it
    /// does not go through [`write_child`]'s indirect-reference check the
    /// way a *child* position would) and inlines just the stream's
    /// dictionary — `<< ... >>` with no `stream`/`endstream` framing and no
    /// `/Length`-last repositioning. That is not what qpdf's real
    /// stream-writing call produces at this position; this primitive simply
    /// does not implement qpdf's stream-writing path
    /// (`QPDFWriter::unparseObject` entered with `f_stream` flags). The
    /// dedicated primitive for that, `unparse_stream_body`, lands in
    /// a later task of this same plan (flpdf-egzr.3.2.13 Task 6); until
    /// then, calling `unparse_object` directly on a stream-resolving handle
    /// is an underspecified, undocumented-by-qpdf shape whose current output
    /// is pinned, in `unparse_object_tests`, by
    /// `unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary`
    /// rather than derived from any qpdf oracle.
    #[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
    pub(crate) fn unparse_object(&self, out: &mut Vec<u8>) -> Result<()> {
        unparse_object_walk(self, out)
    }

    /// QDF-mode counterpart of [`Self::unparse_object`] — same qpdf function
    /// and the same call shape (`QPDFWriter::unparseObject`,
    /// `QPDFWriter.cc:1318-1527`, `level=0, flags=0`), but with the writer's
    /// own `m->qdf_mode` member set to `true` rather than `false` — a mode
    /// flag `unparseObject` checks internally, not an alternate set of call
    /// arguments. Carries forward this port's existing split between compact
    /// and QDF container framing (`Object::write_pdf` / `Object::write_pdf_qdf`, this crate's
    /// `object.rs`; see `docs/qpdf-correspondence.md`'s `QPDFWriter.cc` row,
    /// classified 🔀, for that split) rather than re-deriving the indent
    /// arithmetic from scratch: `indent` is the column (number of leading
    /// spaces) at which *this* value's own opening delimiter sits, an array
    /// or dictionary's children are written at `indent + 2`, and its closing
    /// delimiter (`]` / `>>`) returns to column `indent` on its own line —
    /// exactly [`Object::write_pdf_qdf`]'s own documented contract. Every
    /// scalar (including a resolved-indirect [`ObjectValue::Reference`], no
    /// qpdf counterpart, same as [`Self::unparse_object`]'s own choice for
    /// it) writes byte-identically to the non-QDF form; only array,
    /// dictionary, and stream-dictionary-inlining framing differ.
    ///
    /// Applies the exact same null-suppression rule as [`Self::unparse_object`]
    /// (dictionary entries only — `QPDFWriter.cc:1490-1491`; an array keeps
    /// null elements verbatim, `QPDF_Array::unparse` has no such rule) via
    /// the same [`visible_dict_entries`] helper, and the same forced
    /// top-level resolution of `self` before dispatch. See
    /// [`Self::unparse_object`]'s own doc for the identical
    /// indirect-handle-resolving-to-a-`Stream` caveat: this call dispatches
    /// on `self` directly, bypassing the child-position reference check, so
    /// it inlines just the dictionary rather than implementing qpdf's real
    /// stream-writing framing. The dedicated primitive for *this* (QDF-mode)
    /// shape is [`Self::unparse_stream_body_qdf`] -- not
    /// [`Self::unparse_stream_body`], which has no `indent` parameter and
    /// only ever produces the compact single-line form; that one is the
    /// dedicated primitive for [`Self::unparse_object`]'s own (non-QDF)
    /// identical caveat instead. Do not conflate the two when fixing this
    /// shape at a real call site.
    #[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
    pub(crate) fn unparse_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        unparse_object_walk_qdf(self, indent, out)
    }

    /// Writer-emission counterpart that rewrites indirect child references
    /// through the caller's output-number map without materializing an
    /// [`Object`]. The handle graph remains the source of truth; the callback
    /// only changes the reference token written at each child position.
    #[allow(dead_code)] // compatibility wrapper; canonical writers use the removed-aware sibling
    pub(crate) fn unparse_object_with_ref_map(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    ) -> Result<()> {
        self.unparse_object_with_ref_map_and_removed(out, map, &BTreeSet::new())
    }

    /// Writer-emission counterpart that additionally treats references in
    /// `removed_refs` as qpdf nulls. This is the canonical equivalent of
    /// `renumber_qpdf_refs_in_place_with_removed` for live handle graphs: an
    /// array keeps the position as `null`, while dictionary visibility drops
    /// the null-valued key.
    pub(crate) fn unparse_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        unparse_object_walk_with_ref_map(self, out, map, removed_refs)
    }
}

// The sole recursion hub for `ObjectHandle::materialize` — every nested
// descent (array items, dictionary values, a stream's own dictionary handle)
// goes through this function via `materialize_child`, so the depth cap and
// `stacker::maybe_grow` wrap apply uniformly regardless of which container
// shape carries the nesting. `depth` past `parser::MAX_PARSE_DEPTH`
// substitutes `Object::Null`: no document this crate accepts could parse a
// value nested deeper than that, so only a tree built directly through the
// public `ObjectHandle::array`/`dictionary` factories (which impose no depth
// bound themselves) can reach the cap at all.
fn materialize_bounded(handle: &ObjectHandle, depth: usize) -> Result<Object> {
    if depth > crate::parser::MAX_PARSE_DEPTH {
        return Ok(Object::Null);
    }
    let stream = handle.with_value(|value| match value {
        Some(ObjectValue::Stream {
            stream_dict,
            stream_data,
            ..
        }) => Some((stream_dict.clone(), stream_data.clone())),
        _ => None,
    });
    if let Some((stream_dict, stream_data)) = stream {
        let dict = match materialize_bounded(&stream_dict, depth + 1)? {
            Object::Dictionary(dict) => dict,
            _ => Dictionary::new(),
        };
        let data = match stream_data {
            Some(data) => data.as_ref().clone(),
            None => handle.get_raw_stream_data()?.as_ref().clone(),
        };
        return Ok(Object::Stream(Stream::new(dict, data)));
    }
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        handle.with_value(|value| match value {
            Some(value) => materialize_value(value, depth),
            None => Ok(Object::Null),
        })
    })
}

fn materialize_value(value: &ObjectValue, depth: usize) -> Result<Object> {
    Ok(match value {
        ObjectValue::Null => Object::Null,
        ObjectValue::Boolean(b) => Object::Boolean(*b),
        ObjectValue::Integer(n) => Object::Integer(*n),
        ObjectValue::Real(r) => Object::Real(*r),
        ObjectValue::RealLiteral { value, literal } => Object::RealLiteral {
            value: *value,
            literal: literal.clone(),
        },
        ObjectValue::Name(name) => Object::Name(name.clone()),
        ObjectValue::String(s) => Object::String(s.clone()),
        ObjectValue::Operator(bytes) => Object::Operator(bytes.clone()),
        ObjectValue::InlineImage(bytes) => Object::InlineImage(bytes.clone()),
        ObjectValue::Array(children) => Object::Array(
            children
                .iter()
                .map(|child| materialize_child(child, depth + 1))
                .collect::<Result<Vec<_>>>()?,
        ),
        ObjectValue::Dictionary(entries) => {
            let mut dict = Dictionary::new();
            for (key, value) in entries {
                dict.insert(
                    legacy_dictionary_key(key),
                    materialize_child(value, depth + 1)?,
                );
            }
            Object::Dictionary(dict)
        }
        ObjectValue::Stream { .. } => {
            return Err(Error::Internal(
                "stream materialization must retain its ObjectHandle source".to_owned(),
            ));
        }
        ObjectValue::Reference(object_ref) => Object::Reference(*object_ref),
    })
}

// An array/dictionary child handle materializes to `Object::Reference`
// without recursing into it when indirect (identity-preserving, matching
// the parser's pre-existing `Object::Reference` semantics); a direct child
// is materialized in place. Mirrors `QPDFWriter::unparseChild`
// (`libqpdf/QPDFWriter.cc:1144-1156`), whose `child.isIndirect()` check
// (`:1149`) is the only thing that decides reference-vs-recurse: it never
// inspects what the referenced object resolves to, so an indirect object
// that happens to be qpdf's reserved sentinel (`QPDF_Reserved`) is written
// as an ordinary `"N G R"` reference like any other, never dereferenced,
// here. This has no separate reserved check for the same reason: a reserved
// handle is always indirect by construction
// (`ObjectHandle::new_reserved_for_pdf` always pairs `ObjectState::Reserved`
// with a freshly allocated `object_ref`, and every place that later clears
// `object_ref` -- `disconnect`/`remove_from_document` -- first transitions
// the state away from `Reserved`), so it always takes the `Some` arm below.
// Its own unparseable body is rejected only where qpdf's own equivalent
// throw is reached: dereferencing the reserved object directly, e.g.
// [`ObjectHandle::materialize`]'s own `is_reserved` check, never merely
// because it is reachable as someone else's child.
fn materialize_child(handle: &ObjectHandle, depth: usize) -> Result<Object> {
    Ok(match handle.object_ref() {
        Some(object_ref) => Object::Reference(object_ref),
        None => materialize_bounded(handle, depth)?,
    })
}

// A separate materialization walk used only by `ObjectHandle::unparse_resolved`,
// not by the general `materialize`/`Pdf::resolve_borrowed` bridge above (whose
// existing behavior other callers depend on unchanged). Applies qpdf's
// dictionary-entry null-omission rule (`QPDF_Dictionary::unparse()`,
// `libqpdf/QPDF_Dictionary.cc:59-69`: `if (!iter.second.isNull()) { ... }`) —
// an explicit null value is equivalent to a missing key. `QPDF_Array::unparse()`
// (`libqpdf/QPDF_Array.cc:123-140`) has no such rule; array elements keep
// their null values verbatim, so only the `Dictionary` arm differs from
// `materialize_value` above.
fn unparse_materialize_value(value: &ObjectValue) -> Object {
    match value {
        ObjectValue::Array(children) => {
            Object::Array(children.iter().map(unparse_materialize_child).collect())
        }
        ObjectValue::Dictionary(entries) => {
            let mut dict = Dictionary::new();
            for (key, value) in entries {
                if unparse_is_known_null(value) {
                    continue;
                }
                dict.insert(legacy_dictionary_key(key), unparse_materialize_child(value));
            }
            Object::Dictionary(dict)
        }
        ObjectValue::Stream {
            stream_dict,
            stream_data,
            ..
        } => {
            let dict = match unparse_materialize(stream_dict) {
                Object::Dictionary(dict) => dict,
                _ => Dictionary::new(), // cov:ignore: same invariant as materialize_value's own Stream arm
            };
            // Same legacy-route payload copy as `materialize_value`'s arm.
            Object::Stream(Stream::new(
                dict,
                stream_data
                    .as_ref()
                    .expect("unparse requires replaced stream data")
                    .as_ref()
                    .clone(),
            ))
        }
        // No other variant nests a dictionary, so the omission rule cannot
        // apply anywhere beneath it; delegate to the ordinary materializer.
        // Every remaining variant is a scalar with no further recursion, so
        // the depth this arm passes never actually matters.
        other => {
            materialize_value(other, 0).expect("non-stream unparse materialization is infallible")
        }
    }
}

// Stack-safety constants for `unparse_materialize`'s recursive walk,
// mirroring `parser.rs`'s own `STACK_RED_ZONE`/`STACK_GROWTH_SIZE` values
// (kept as separate local constants rather than imported cross-module,
// since this slice's own scope is limited to this file). A tree built
// directly through the public `ObjectHandle::array`/`dictionary` factories
// carries no depth bound the way parsed input does (`parser::MAX_PARSE_DEPTH`
// rejects a document too deep to parse before an `ObjectHandle` tree that
// deep can even exist for it), so this walk needs the same stack-growth
// protection the parser already relies on for its own recursion.
const UNPARSE_STACK_RED_ZONE: usize = 32 * 1024;
const UNPARSE_STACK_GROWTH_SIZE: usize = 1024 * 1024;

// The sole recursion hub for `unparse_materialize_value`'s `Array`/
// `Dictionary`/`Stream` arms (every nested descent goes through
// `unparse_materialize_child`, which calls back into this function) --
// wrapping recursion here, in one place, bounds every nesting path the same
// way `parser::Parser::object`'s own single hub does for parsing.
fn unparse_materialize(handle: &ObjectHandle) -> Object {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        handle.with_value(|value| match value {
            Some(value) => unparse_materialize_value(value),
            None => Object::Null,
        })
    })
}

fn unparse_materialize_child(handle: &ObjectHandle) -> Object {
    match handle.object_ref() {
        Some(object_ref) => Object::Reference(object_ref),
        None => unparse_materialize(handle),
    }
}

fn reserved_unparse_error() -> Error {
    Error::System("QPDFObjectHandle: attempting to unparse a reserved object".to_owned())
}

// Writes one child handle's bytes for the plain-unparse family serviced by
// `unparse_object_walk` below: an indirect child always writes as its own
// `"N G R"` reference form, never recursed into — the same reference-vs-
// recurse split `materialize_child`/`unparse_materialize_child` above already
// apply, mirroring `QPDFWriter::unparseChild`'s own `child.isIndirect()`
// check (`libqpdf/QPDFWriter.cc:1144-1156`, the check itself at `:1149`),
// which `unparseObject`'s array-element and dictionary-value loops call into
// for exactly this decision (`:1342`, `:1503`) instead of inlining it. A
// direct child recurses through `unparse_object_walk`.
//
// No separate reserved check, for the same reason `materialize_child` has
// none (see its own doc): a reserved handle is always indirect, so it always
// takes the reference-token branch below without ever being dereferenced
// here, matching `unparseChild`'s own `isIndirect()`-only decision, which
// never inspects the referenced object's resolved type either. Its own
// unparseable body is rejected only when dereferenced directly -- `self` at
// [`ObjectHandle::unparse_object`]'s top level, handled by
// `unparse_object_walk`'s own `is_reserved` check -- not merely because it
// is reachable as someone else's child through this function.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn write_child(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk(handle, out)
}

// Filters `entries` down to the ones `unparseObject`'s dictionary branch
// would actually write (`QPDFWriter.cc:1490-1491`). Forces resolution of
// every indirect *value* via `try_is_null` to decide suppression -- this is
// the one place in this primitive family that performs that particular
// hidden I/O qpdf's own `isNull()` performs and every other *value*
// accessor in this file deliberately avoids (see `unparse_resolved`'s own
// doc on why *it* does not resolve on the caller's behalf).
// `unparse_object_walk` separately forces resolution of `self` -- a
// different target, for a different reason: dispatching on `self`'s own
// resolved type, not deciding whether to suppress it. Neither forced
// resolution is a contract violation here: `QPDFWriter::unparseObject` is a
// writer-internal path with no no-hidden-I/O constraint to begin with.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn visible_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
) -> Result<Vec<(&Vec<u8>, &ObjectHandle)>> {
    let mut visible = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if !value.try_is_null()? {
            visible.push((key, value));
        }
    }
    Ok(visible)
}

fn is_removed_reference(handle: &ObjectHandle, removed_refs: &BTreeSet<ObjectRef>) -> bool {
    handle
        .object_ref()
        .is_some_and(|object_ref| removed_refs.contains(&object_ref))
        || handle
            .as_reference()
            .is_some_and(|object_ref| removed_refs.contains(&object_ref))
}

// The sole recursion hub for the plain unparse family (`ObjectHandle::
// unparse_object` and its callees below), mirroring `unparse_materialize`'s
// own single-hub pattern above for the same stack-growth reason: an
// `ObjectHandle` tree built through public factories carries no depth bound
// the parser enforces on parsed input. Also forces resolution of `handle`
// itself before inspecting its value: every call into this hub either comes
// from `unparse_object`'s top-level entry point (whose argument may still be
// an unresolved indirect handle) or from a direct child that `write_child`
// has already filtered past its own indirect check (so `handle` here is
// always already direct in that case, making the call a no-op) — mirroring
// qpdf's own implicit `dereference()` on `object`'s first `isXxx()` type
// check inside `unparseObject` itself, rather than the no-hidden-I/O
// contract [`ObjectHandle::with_value`]'s other callers rely on.
enum UnparseContainer {
    Array(Vec<ObjectHandle>),
    Dictionary(Vec<(Vec<u8>, ObjectHandle)>),
    Stream(ObjectHandle),
}

// qpdf's writer walks a live container and does not clone scalar payloads.
// The RefCell borrow must nevertheless be released before a child is resolved,
// since resolution can mutate the same shared state. Snapshot only the edges
// needed for a later recursive descent; scalar/name/string bytes are emitted
// while their borrow is still active.
fn snapshot_unparse_container(value: &ObjectValue) -> Option<UnparseContainer> {
    match value {
        ObjectValue::Array(children) => Some(UnparseContainer::Array(children.clone())),
        ObjectValue::Dictionary(entries) => Some(UnparseContainer::Dictionary(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )),
        ObjectValue::Stream { stream_dict, .. } => {
            Some(UnparseContainer::Stream(stream_dict.clone()))
        }
        _ => None,
    }
}

fn unparse_container(container: UnparseContainer, out: &mut Vec<u8>) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child(&child, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        UnparseContainer::Dictionary(entries) => unparse_dict_entries(&entries, out)?,
        UnparseContainer::Stream(stream_dict) => {
            // This primitive inlines only a stream's dictionary; stream
            // framing remains `unparse_stream_body`'s responsibility.
            unparse_object_walk(&stream_dict, out)?;
        }
    }
    Ok(())
}

#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_object_walk(handle: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    // Scalars have no child to resolve, so serialize them
                    // while the borrow is active instead of cloning payloads.
                    unparse_object_value(value, out).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: unreachable once `try_dereference()`
                // above has returned `Ok`; retain the conservative null
                // fallback for a resolver that violates that invariant.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container(container, out),
            None => Ok(()),
        }
    })
}

#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_object_value(value: &ObjectValue, out: &mut Vec<u8>) -> Result<()> {
    match value {
        ObjectValue::Null => out.extend_from_slice(b"null"),
        ObjectValue::Boolean(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        ObjectValue::Integer(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ObjectValue::Real(v) => out.extend_from_slice(v.to_string().as_bytes()),
        ObjectValue::RealLiteral { value, literal } => {
            if crate::object::real_literal_is_safe(literal, *value) {
                out.extend_from_slice(literal);
            } else {
                out.extend_from_slice(value.to_string().as_bytes());
            }
        }
        ObjectValue::Name(name) => {
            out.push(b'/');
            crate::object::write_name_escaped(out, name);
        }
        ObjectValue::String(value) => crate::object::write_string_value(out, value),
        ObjectValue::Operator(value) | ObjectValue::InlineImage(value) => {
            out.extend_from_slice(value);
        }
        ObjectValue::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child(child, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries(&entries, out)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            // Reachable two ways, not just one: a *direct* Stream value (no
            // qpdf counterpart -- a real QPDFObjectHandle's resolved value
            // is never itself a stream outside an indirect object), and an
            // *indirect* `self` at the top level of `unparse_object` that
            // resolves to a stream (a real, reachable qpdf shape -- see
            // `ObjectHandle::unparse_object`'s own doc). The latter is
            // reachable here because `unparse_object`/`unparse_object_walk`
            // call this dispatch directly on `self`, bypassing `write_child`
            // entirely; `write_child` only gates *child* positions (array
            // elements, dictionary values) during recursion, where it never
            // recurses into an indirect handle -- so an *indirect* child
            // resolving to a stream short-circuits to its own `"N G R"`
            // form and never reaches this arm. A *direct* child whose value
            // is a Stream does reach it, by the first case above.
            //
            // Either way, this arm inlines only the dictionary, deliberately
            // not the `stream`/`endstream` framing: that framing (and the
            // `/Length`-last, optionally re-filtered stream-dictionary
            // layout it wraps) is `unparse_stream_body`'s own, separately
            // scoped responsibility (flpdf-egzr.3.2.13 Task 6) -- this
            // generic dispatch does not implement qpdf's real
            // stream-writing path for the indirect case either.
            unparse_object_walk(stream_dict, out)?;
        }
        ObjectValue::Reference(object_ref) => {
            // qpdf-cutover-delete(flpdf-25kg.3.3) variant: an indirect
            // handle's own resolved value can genuinely be a bare reference
            // (e.g. a `Pdf::set_object` redirect -- see `ObjectValue::
            // Reference`'s own doc; exercised by `unparse_tests::
            // resolved_to_a_reference_indirect_handle_unparse_and_unparse_resolved_diverge`).
            // No qpdf counterpart exists (a real `QPDFObjectHandle`'s
            // resolved value is never itself a bare reference), so there is
            // no oracle to match byte-for-byte; this mirrors
            // `unparse_resolved`'s own choice for the identical shape
            // (`unparse_materialize_value`'s fallthrough to
            // `materialize_value`'s `Reference` arm, then
            // `Object::write_pdf`, `object.rs:544-546`) rather than
            // silently writing nothing.
            out.extend_from_slice(object_ref.to_string().as_bytes());
        }
    }
    Ok(())
}

type ObjectRefMap<'a> = dyn Fn(ObjectRef) -> Result<ObjectRef> + 'a;

// Ref-map sibling of `write_child` above -- same reference-vs-recurse split
// on `handle.object_ref()` alone, so the same reasoning applies: a reserved
// child is always indirect and therefore always takes this `Some` branch
// (writing its mapped reference token, or `null` if renumbering removed it,
// per the qpdf-rewrite null-handling below) without ever being dereferenced
// here. See `write_child`'s own doc for why no separate reserved check
// belongs in a child-position function at all.
fn write_child_with_ref_map(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        if object_ref.number == 0 || removed_refs.contains(&object_ref) {
            // qpdf's direct-null identity is object number zero, not an
            // output reference (QPDFObjectHandle.cc:344-350). A removed
            // identity follows the same null path in the qpdf rewrite.
            out.extend_from_slice(b"null");
            return Ok(());
        }
        let mapped = map(object_ref)?;
        out.extend_from_slice(mapped.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk_with_ref_map(handle, out, map, removed_refs)
}

fn unparse_object_walk_with_ref_map(
    handle: &ObjectHandle,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        // A resolved indirect redirect stores its reference as a scalar, but
        // the mapping callback may re-enter mutation of this same handle.
        // Copy the small reference token out before invoking the callback so
        // `with_value`'s RefCell borrow cannot cross that call.
        let reference = handle.with_value(|value| match value {
            Some(ObjectValue::Reference(object_ref)) => Some(*object_ref),
            _ => None,
        });
        if let Some(object_ref) = reference {
            if object_ref.number == 0 || removed_refs.contains(&object_ref) {
                out.extend_from_slice(b"null");
            } else {
                let mapped = map(object_ref)?;
                out.extend_from_slice(mapped.to_string().as_bytes());
            }
            return Ok(());
        }
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    // Scalar payloads are written under the borrow; only
                    // container edges need an owned snapshot before descent.
                    unparse_object_value_with_ref_map(value, out, map, removed_refs).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes Null for
                // missing states or errors while unresolved.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_with_ref_map(container, out, map, removed_refs),
            None => Ok(()),
        }
    })
}

fn unparse_container_with_ref_map(
    container: UnparseContainer,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child_with_ref_map(&child, out, map, removed_refs)?;
            }
            out.extend_from_slice(b" ]");
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_with_ref_map(&entries, out, map, removed_refs)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_with_ref_map(&stream_dict, out, map, removed_refs)?;
        }
    }
    Ok(())
}

fn unparse_object_value_with_ref_map(
    value: &ObjectValue,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    match value {
        ObjectValue::Array(children) => {
            out.push(b'[');
            for child in children {
                out.push(b' ');
                write_child_with_ref_map(child, out, map, removed_refs)?;
            }
            out.extend_from_slice(b" ]");
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries_with_ref_map(&entries, out, map, removed_refs)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            unparse_object_walk_with_ref_map(stream_dict, out, map, removed_refs)?;
        }
        ObjectValue::Reference(object_ref) => {
            if object_ref.number == 0 || removed_refs.contains(object_ref) {
                out.extend_from_slice(b"null");
            } else {
                let mapped = map(*object_ref)?;
                out.extend_from_slice(mapped.to_string().as_bytes());
            }
        }
        _ => unparse_object_value(value, out)?,
    }
    Ok(())
}

fn unparse_dict_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_with_ref_map(value, out, map, removed_refs)?;
        }
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Detects the sibling condition `QPDFWriter::unparseObject`'s dictionary
// branch checks per key before special-casing `/Contents`
// (`QPDFWriter.cc:1497-1498`: `object.isDictionaryOfType("/Sig") &&
// object.hasKey("/ByteRange")`) -- `object` there is the dict *being
// written* (this function's own `entries`), not the `/Contents` value
// itself. Checked in qpdf's own short-circuit order: `/Type` first, then
// `/ByteRange` only if `/Type` was `/Sig` -- `isDictionaryOfType`
// (`QPDFObjectHandle.cc:461-466`) resolves `/Type`'s own value through
// `getKey("/Type").isNameAndEquals("/Sig")` (`isNameAndEquals` calls
// `isName()`, which dereferences), so an indirect `/Type` value is
// force-resolved here too, matching that (the suppression predicate above,
// `visible_dict_entries`, already accepts this same "no-hidden-I/O
// constraint" tradeoff for the identical writer-internal reason -- see its
// own doc).
//
// `hasKey` is **not** pure map-containment despite its name:
// `QPDFObjectHandle::hasKey` (`QPDFObjectHandle.cc:965-976`) delegates to
// `QPDF_Dictionary::hasKey` (`QPDF_Dictionary.cc:98-101`), which is
// `items.count(key) > 0 && !items[key].isNull()` -- `isNull()`
// (`QPDFObjectHandle.cc:353-356`) dereferences too, so a `/ByteRange` key
// whose value resolves to null (directly or indirectly) counts as *absent*,
// the same null-suppression rule `visible_dict_entries` already applies to
// dict entries generally. `/ByteRange`'s own value is therefore
// force-resolved here as well -- but only after `/Type` was already
// confirmed `/Sig`, matching qpdf's `&&` short-circuit: a dict whose
// `/Type` is not `/Sig` never touches `/ByteRange`'s resolver at all.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn dict_is_sig_with_byte_range(entries: &[(Vec<u8>, ObjectHandle)]) -> Result<bool> {
    let Some((_, type_value)) = entries.iter().find(|entry| entry.0.as_slice() == b"/Type") else {
        return Ok(false);
    };
    type_value.try_dereference()?;
    let is_sig = type_value.with_value(
        |value| matches!(value, Some(ObjectValue::Name(name)) if name.as_slice() == b"Sig"),
    );
    if !is_sig {
        return Ok(false);
    }
    let Some((_, byte_range_value)) = entries
        .iter()
        .find(|entry| entry.0.as_slice() == b"/ByteRange")
    else {
        return Ok(false);
    };
    Ok(!byte_range_value.try_is_null()?)
}

// Applies qpdf's `/Contents`-in-a-signature-dictionary hex-string special
// case (`QPDFWriter.cc:1490-1504`) to a single dict-value child in place of
// the ordinary `write_child`/`write_child_qdf` call, when `force_hex_string`
// is set (every call site below passes `key.as_slice() == b"Contents" &&
// dict_is_sig_with_byte_range(entries)?`, matching the `key == "/Contents" &&
// object.isDictionaryOfType(...) && object.hasKey(...)` guard at the same
// source lines). Returns `Ok(true)` when it wrote the value itself -- the
// caller must not also call the ordinary child-writer in that case -- or
// `Ok(false)` when the ordinary path should run instead.
//
// The key check must come *first* in that `&&`, not merely for a byte-for-byte
// mirror of qpdf's own operand order: qpdf's guard sits inside the *same*
// per-item loop that already visits every key for null-suppression
// (`:1488-1491`), so `isDictionaryOfType`/`hasKey`'s own resolution of
// `/Type`/`/ByteRange` only ever runs when that loop's *current* item is
// literally `/Contents` -- a dict with no `/Contents` key never reaches it at
// all. Every call site below evaluates `dict_is_sig_with_byte_range(entries)?`
// on that same short-circuited, per-key-gated basis -- once, lazily, only if
// and when the loop below actually reaches a surviving `/Contents` key --
// rather than once, unconditionally, before the loop starts.
//
// Note what this ordering fix does *not* claim: unlike Finding 2's
// `refiltered`-key exclusion (see `unparse_stream_dict_entries`'s own doc),
// this is not a "never touched at all" guarantee for `/Type`/`/ByteRange`.
// Both remain ordinary surviving dict keys -- unlike `/Filter`/`/DecodeParms`
// under `refiltered`, they are never removed from `entries` -- so
// `visible_dict_entries`'s own generic per-item null check
// (`:1488`/`isNull()`, mirrored here) force-resolves them anyway whenever
// they are present, independent of whether `/Contents` exists at all. What
// hoisting this call above the loop (as this function previously did)
// actually changes is *ordering*: it force-resolves `/Type` (and,
// conditionally, `/ByteRange`) *before* the null-suppression pass runs at
// all, so a dict whose surviving keys straddle `/Type` in the dict's own
// (`BTreeMap`) alphabetical order surfaces `/Type`'s own resolution error
// ahead of an earlier-sorting key's error that qpdf's single-pass loop would
// have reached first. Gating the call on the loop's *current* key, as fixed
// here, keeps that resolution order aligned with qpdf's own single pass
// (code-quality review of commit 6cae41fd; see
// `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
// below, which pins this ordering against a dict with no `/Contents` key at
// all -- and documents, in its own comment, why a plain success-vs-error
// assertion cannot observe this fix at all).
//
// Matches `unparseChild`'s own indirect-first short-circuit
// (`QPDFWriter.cc:1149-1156`): an indirect child still writes as its own
// `"N G R"` reference form regardless of the flag -- real qpdf's flags are
// consulted only inside `unparseObject`, which `unparseChild` never reaches
// for an indirect child at all -- so this only ever has an effect on a
// *direct* child. Even then, it only affects a child whose resolved value is
// itself a String: qpdf's own `f_hex_string` handling lives inside
// `unparseObject`'s `ot_string` arm alone (`QPDFWriter.cc:1567,1594-1595`);
// every other resolved type's arm never inspects the flag, so a non-String
// direct child (unusual for `/Contents` in practice, but not structurally
// ruled out) falls through to the ordinary child-writer unaffected, matching
// that.
//
// Deliberately does not implement `f_no_encryption` (`QPDFWriter.cc:1501`)
// -- qpdf's `ot_string` arm consults it only inside its own `m->encrypted`
// branch (`:1569-1593`), routing this one child's bytes through a
// non-encrypting sub-pipeline while the rest of the document is encrypted.
// This crate's `ObjectHandle` writer-emission primitives carry no
// pipeline/encryption context at all -- every one of them is a plain
// `(&self, out: &mut Vec<u8>, ...) -> Result<()>` -- so there is no
// encryption state to route around in the first place here; wiring an
// actual encryption pipeline around these bytes is a future
// consumer-migration/encryption-integration concern this primitive does not
// implement, matching the scope limits `unparse_stream_body`/
// `unparse_trailer` already document for their own out-of-scope qpdf steps
// (e.g. the `t_lin_second` branch, the `/Crypt`-filter stripping logic).
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn try_write_sig_contents_hex_string(
    handle: &ObjectHandle,
    force_hex_string: bool,
    out: &mut Vec<u8>,
) -> Result<bool> {
    if !force_hex_string || handle.object_ref().is_some() {
        return Ok(false);
    }
    handle.try_dereference()?;
    Ok(handle.with_value(|value| {
        if let Some(ObjectValue::String(bytes)) = value {
            crate::object::write_hex_string(out, bytes);
            true
        } else {
            false
        }
    }))
}

// Writes `<< /K1 v1 /K2 v2 >>` with qpdf's suppression rule applied
// (`QPDFWriter.cc:1488-1527`, non-stream case: no `/Length` tail). Matches
// `Dictionary::write_pdf`'s own key-writing shape (`object.rs:839-848`): a
// leading space, then `/` + the escaped key, pushed separately since
// `write_name_escaped` does not write the leading slash itself. Also applies
// the `/Contents`-in-a-`/Sig`-dictionary hex-string special case that same
// qpdf loop applies unconditionally (`QPDFWriter.cc:1490-1504`) -- see
// `dict_is_sig_with_byte_range`/`try_write_sig_contents_hex_string`'s own
// docs for the detection/writing split (Codex Review on PR #644,
// crates/flpdf/src/object_handle.rs:2087).
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_dict_entries(entries: &[(Vec<u8>, ObjectHandle)], out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in visible_dict_entries(entries)? {
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child(value, out)?;
        }
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Append `n` ASCII space bytes to `out` — the QDF family's own copy of
// `object.rs`'s private `push_spaces` helper. Not reusable across the module
// boundary (that one is not `pub(crate)`, and this task's scope is
// `object_handle.rs` only), but the two are one-line bodies, not logic worth
// sharing at the cost of widening `object.rs`'s API for a single call site.
fn push_spaces(out: &mut Vec<u8>, n: usize) {
    out.resize(out.len() + n, b' ');
}

// QDF-mode sibling of `write_child` above: an indirect child always writes
// as its own `"N G R"` reference form regardless of QDF mode — qpdf never
// inlines an indirect object at a child position in either mode, the same
// unconditional split `unparse_materialize_child` already applies. A direct
// child recurses through `unparse_object_walk_qdf` at `indent`, the same
// column its own container already committed to for this child (an array
// element or dict value sits at its container's `indent + 2`; see
// `unparse_object_value_qdf`'s own Array/Dictionary arms for where that
// `+ 2` is actually applied before calling this).
//
// No separate reserved check either, for the identical reason `write_child`
// has none: a reserved child is always indirect, so it always takes the
// reference-token branch below without ever being dereferenced here.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn write_child_qdf(handle: &ObjectHandle, indent: usize, out: &mut Vec<u8>) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        out.extend_from_slice(object_ref.to_string().as_bytes());
        return Ok(());
    }
    unparse_object_walk_qdf(handle, indent, out)
}

// QDF-mode sibling of `unparse_object_walk` above, threading an `indent`
// column through the same forced-top-level-resolution / stack-growth-wrapped
// recursion hub shape. See that function's own doc for why `try_dereference`
// is forced here rather than left to `with_value`'s ordinary no-hidden-I/O
// contract, and for the same conservative-null fallback rationale on the
// `None` arm below.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_object_walk_qdf(handle: &ObjectHandle, indent: usize, out: &mut Vec<u8>) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    // QDF changes container framing only; scalar bytes take
                    // the same no-copy path as compact unparse.
                    unparse_object_value_qdf(value, indent, out).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: unreachable once `try_dereference()`
                // above has returned `Ok` -- see `unparse_object_walk`'s own
                // identical arm for why.
                out.extend_from_slice(b"null");
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_qdf(container, indent, out),
            None => Ok(()),
        }
    })
}

fn unparse_container_qdf(
    container: UnparseContainer,
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            // Object::write_pdf_qdf's Array arm: `[`, a newline, then each
            // child at `indent + 2`, followed by the closing bracket at
            // `indent`.
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf(&child, indent + 2, out)?;
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf(&entries, indent, out)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_qdf(&stream_dict, indent, out)?;
        }
    }
    Ok(())
}

// QDF-mode sibling of `unparse_object_value` above. Only the container arms
// (`Array`, `Dictionary`, the `Stream` dictionary-inlining arm) differ from
// the plain form -- every scalar/name/string/reference arm is byte-identical
// between the two modes (`Object::write_pdf_qdf`'s own fallthrough to
// `self.write_pdf(out)` for everything but its three container arms is the
// same split), so this delegates that whole fallthrough set to
// `unparse_object_value` itself rather than duplicating its match arms.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_object_value_qdf(value: &ObjectValue, indent: usize, out: &mut Vec<u8>) -> Result<()> {
    match value {
        ObjectValue::Array(children) => {
            // Object::write_pdf_qdf's Array arm (object.rs): `[`, a newline,
            // then per element `indent + 2` leading spaces + the child's own
            // QDF form + a trailing newline, then `indent` leading spaces and
            // `]`.
            out.push(b'[');
            out.push(b'\n');
            for child in children {
                push_spaces(out, indent + 2);
                write_child_qdf(child, indent + 2, out)?;
                out.push(b'\n');
            }
            push_spaces(out, indent);
            out.push(b']');
        }
        ObjectValue::Dictionary(entries) => {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries_qdf(&entries, indent, out)?;
        }
        ObjectValue::Stream { stream_dict, .. } => {
            // Same reachability and "inlines only the dictionary" caveat as
            // `unparse_object_value`'s own `Stream` arm (see its doc) --
            // but note that arm's doc names `unparse_stream_body` (the
            // *compact* primitive) as the dedicated responsible primitive
            // for its own caveat; for *this* QDF arm the dedicated
            // primitive is `unparse_stream_body_qdf` instead, not that one
            // (it has no `indent` parameter and only ever produces the
            // compact single-line form). This recurses into the stream's
            // dictionary handle at the *same* `indent`, not `indent + 2` --
            // a stream dictionary is not a child sitting inside a container
            // the way an array element or dict value is; it occupies this
            // same value's own position, exactly as `Object::write_pdf_qdf`'s
            // `Stream` arm calls `stream.dict.write_pdf_qdf(out, indent)` at
            // the unincremented indent before appending its
            // `stream`/`endstream` framing.
            unparse_object_walk_qdf(stream_dict, indent, out)?;
        }
        // Every remaining variant (Null, Boolean, Integer, Real, RealLiteral,
        // Name, String, Operator, InlineImage, Reference) has no QDF-specific
        // framing -- reuse `unparse_object_value`'s own arms for them
        // verbatim rather than duplicating scalar-formatting logic. Spelled
        // out explicitly (rather than an `other =>` catch-all) so this match
        // stays exhaustive: adding a new `ObjectValue` variant, or removing
        // one of the three container arms above, is a compile error here
        // instead of a silent fallthrough -- the same enforcement
        // `unparse_object_value` itself already gets from having no
        // catch-all arm at all.
        ObjectValue::Null
        | ObjectValue::Boolean(_)
        | ObjectValue::Integer(_)
        | ObjectValue::Real(_)
        | ObjectValue::RealLiteral { .. }
        | ObjectValue::Name(_)
        | ObjectValue::String(_)
        | ObjectValue::Operator(_)
        | ObjectValue::InlineImage(_)
        | ObjectValue::Reference(_) => unparse_object_value(value, out)?,
    }
    Ok(())
}

// QDF-mode sibling of `unparse_dict_entries` above: `<<\n`, then one
// `  /Key value\n` line per surviving entry (indented `indent + 2`, keys in
// the same lexicographic order `visible_dict_entries` preserves), then `>>`
// at column `indent` on its own line -- matches `Dictionary::write_pdf_qdf`'s
// own layout (`object.rs`) exactly, including its documented empty-dictionary
// shape (`<<\n<indent spaces>>>`) when every entry is absent or suppressed.
// Suppression itself is `visible_dict_entries`, unchanged from the plain
// path -- QDF mode does not alter *which* entries survive, only how the
// survivors are laid out. Applies the same `/Contents`-in-a-`/Sig`-dictionary
// hex-string special case `unparse_dict_entries` applies -- real qpdf's own
// guard (`QPDFWriter.cc:1497-1503`) is unconditional across `m->qdf_mode`.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_dict_entries_qdf(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.extend_from_slice(b"<<\n");
    for (key, value) in visible_dict_entries(entries)? {
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_qdf(value, indent + 2, out)?;
        }
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

impl ObjectHandle {
    /// This stream-dictionary handle's writer-emission form, matching
    /// `Dictionary::write_pdf_stream`'s established layout (`object.rs`)
    /// -- the `/Length`-last, optionally re-filtered
    /// stream-dictionary shape `QPDFWriter::unparseObject`'s stream branch
    /// produces when it delegates to its own dictionary branch
    /// (`QPDFWriter.cc:1440-1442` enters with `flags |= f_stream`;
    /// `1451-1455`, only when `refiltered`, drops `/Filter`/`/DecodeParms`;
    /// `1488-1527` is the dictionary-branch loop that writes the surviving
    /// keys, `/Length`, and, when `refiltered`, a fresh `/Filter
    /// /FlateDecode`) -- plus the same null-suppression rule as
    /// [`Self::unparse_object`], since this delegation target is the
    /// identical dictionary branch.
    ///
    /// Like `write_pdf_stream` itself, this primitive does not replicate
    /// every qpdf step in that line range: the unconditional
    /// empty-`/DecodeParms`-array removal (`1444-1449`), the
    /// `/Crypt`-filter stripping in the non-refiltered branch
    /// (`1456-1485`), qpdf's `compress && (flags & f_filtered)` gate on the
    /// trailing `/Filter /FlateDecode` append (`1519`, driven by
    /// `refiltered` alone here), and qpdf's own computed `/Length` *value*
    /// (`1508-1518`: `stream_length`/`cur_stream_length_id`, not the
    /// dictionary's own stored value) are all out of scope -- inherited
    /// unchanged from `write_pdf_stream`'s own established simplifications
    /// (see that function's doc for the full qpdf-correspondence caveat).
    ///
    /// `self` normally resolves to a `Dictionary` directly -- this
    /// primitive's usual caller already holds an already-resolved stream's
    /// dictionary handle (see below). It also accepts `self` resolving to a
    /// `Stream { stream_dict, .. }`, the same shape [`Self::unparse_object`]'s
    /// own `Stream` arm accepts when an indirect handle resolves to a stream
    /// (see that primitive's own doc for why this shape is reachable): in
    /// that case `stream_dict` -- itself an [`ObjectHandle`], not
    /// necessarily already resolved -- is forced to resolve (propagating any
    /// error, e.g. a dropped document, the same way the top-level `self`
    /// resolution below does; see `unparse_stream_body_resolves_an_unresolved_indirect_stream_dict`
    /// and `unparse_stream_body_propagates_a_dropped_document_error_from_stream_dict`,
    /// which fail without this call) and its entries are used exactly as if
    /// `self` had been that dictionary handle to begin with. Any other
    /// resolved shape for `self`, or a `stream_dict` that itself resolves to
    /// something other than a `Dictionary`, degrades to an empty `<< >>`,
    /// mirroring `write_pdf_stream`'s own typed-input assumption (this
    /// crate's writer never calls it on anything else).
    ///
    /// Forces resolution of `self` before dispatch, the same as
    /// [`Self::unparse_object`]/[`Self::unparse_object_qdf`]'s own
    /// top-level entry points -- this primitive's usual caller already
    /// holds an already-resolved stream's dictionary handle, but nothing
    /// enforces that at the type level, and an as-yet-unresolved indirect
    /// handle whose document has been dropped must surface as an error
    /// here too, not silently degrade to an empty `<< >>` the way an
    /// unresolved [`Self::with_value`] read alone would (see
    /// `unparse_stream_body_propagates_a_dropped_document_error`, which
    /// fails without this call).
    #[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
    pub(crate) fn unparse_stream_body(&self, out: &mut Vec<u8>, refiltered: bool) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    // `stream_dict` is itself an `ObjectHandle` that may not
                    // yet be resolved (e.g. a mock-resolver-bearing indirect
                    // handle whose value is a `Stream` wrapping another
                    // indirect dictionary handle) -- force its own
                    // resolution, mirroring the `self.try_dereference()?`
                    // above, before reading its value.
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries(&entries, refiltered, out)
        })
    }

    /// QDF-mode counterpart of [`Self::unparse_stream_body`] -- same
    /// delegation-target dimension as [`Self::unparse_object_qdf`] is to
    /// [`Self::unparse_object`] (`m->qdf_mode` set to `true` inside the
    /// same `QPDFWriter::unparseObject` dictionary branch,
    /// `QPDFWriter.cc:1346-1527`; the `f_stream`/`f_filtered` handling at
    /// `:1440-1455` and the `/Length`-then-`/Filter` tail at `:1508-1524`
    /// run unconditionally there, regardless of `m->qdf_mode` -- only
    /// `indent`/`writeStringQDF` differ between the two modes), matching
    /// `Dictionary::write_pdf_stream_qdf`'s established layout
    /// (`object.rs:1036`) -- multi-line QDF framing (`<<\n`, each
    /// surviving key at `indent + 2` with a trailing `\n`, closing `>>` at
    /// `indent`), with `/Length` pulled out of the iteration and written
    /// last, immediately before `>>` -- plus the same null-suppression
    /// rule as [`Self::unparse_object_qdf`]/[`Self::unparse_stream_body`],
    /// via the same [`visible_dict_entries`] helper.
    ///
    /// Unlike [`Self::unparse_stream_body`], this primitive has **no
    /// `refiltered` parameter** -- matching `Dictionary::write_pdf_stream_qdf`'s
    /// own signature exactly, which has none either. This is not fixed by
    /// the caller already holding a settled `/Filter`/`/Length`: unlike a
    /// stored *value*, `refiltered` in the compact path controls emitted
    /// *key order* (`/Filter` pulled after `/Length` vs. left at its plain
    /// alphabetical position) regardless of what `/Filter` already
    /// contains, so a settled dict does not make the dimension moot on its
    /// own. Real qpdf's `unparseObject` *does* apply the identical
    /// `f_filtered` key-pull-and-reappend logic inside `m->qdf_mode` too
    /// (`QPDFWriter.cc:1451-1455`/`:1519-1522`, the same `if` guards,
    /// unguarded by `qdf_mode`) -- so a genuinely re-filtered stream on the
    /// QDF full-rewrite path is, like `write_pdf_stream_qdf` itself, an
    /// existing, out-of-scope simplification this primitive matches rather
    /// than one this task introduces or is asked to fix: this primitive's
    /// signature simply mirrors its delegation target's real (already
    /// simplified) shape, the same convention every other primitive in
    /// this family follows for the legacy function it ports.
    ///
    /// `self` accepts the same two shapes [`Self::unparse_stream_body`]
    /// does -- a `Dictionary` directly, or a `Stream { stream_dict, .. }`
    /// whose (possibly still-unresolved) `stream_dict` is forced to
    /// resolve -- with the identical error-propagation behavior for every
    /// other shape (degrading to an empty dictionary in this layout's own
    /// `<<\n>>` shape, not the compact sibling's `<< >>`); see that
    /// primitive's own doc for the full contract, which this one mirrors
    /// exactly except for the QDF layout and the missing `refiltered`
    /// parameter.
    #[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
    pub(crate) fn unparse_stream_body_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    // Mirrors `unparse_stream_body`'s identical
                    // `stream_dict.try_dereference()?` -- see that
                    // primitive's own doc for why this is needed rather
                    // than a plain `with_value` read.
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_qdf(&entries, indent, out)
        })
    }

    /// Stream-dictionary writer emission with output reference remapping.
    /// This is the canonical writer boundary: dictionary children are read
    /// from the live handle graph and only the serialized reference tokens are
    /// translated for the new file.
    #[allow(dead_code)] // compatibility wrapper; canonical writers use the removed-aware sibling
    pub(crate) fn unparse_stream_body_with_ref_map(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    ) -> Result<()> {
        self.unparse_stream_body_with_ref_map_and_removed(out, refiltered, map, &BTreeSet::new())
    }

    /// Stream-dictionary writer emission with output reference remapping and
    /// qpdf null visibility for references removed during this write.
    pub(crate) fn unparse_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                Some(ObjectValue::Stream { stream_dict, .. }) => {
                    stream_dict.try_dereference()?;
                    stream_dict.with_value(|dict_value| match dict_value {
                        Some(ObjectValue::Dictionary(entries)) => entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_with_ref_map(&entries, refiltered, out, map, removed_refs)
        })
    }
}

// Writes a stream dictionary's own body -- `unparse_stream_body`'s sole
// callee -- matching `Dictionary::write_pdf_stream`'s established shape
// (`object.rs`) with `visible_dict_entries`'s null-suppression layered on
// top, the same delegation `unparse_dict_entries` above makes to that
// helper for the plain (non-stream) dictionary case. `/Length` is captured
// during the single suppressed-entries pass and written last rather than in
// its natural (sorted) position; when `refiltered`, `/Filter` and
// `/DecodeParms` are dropped from that pass and a fresh `/Filter
// /FlateDecode` is appended after `/Length` instead -- both spellings
// verified byte-for-byte against `write_pdf_stream` (`object.rs`) before
// this primitive's tests were written. Also applies the same
// `/Contents`-in-a-`/Sig`-dictionary hex-string special case
// `unparse_dict_entries` applies -- real qpdf's own guard
// (`QPDFWriter.cc:1497-1503`) has no `f_stream` gate, so in principle it
// covers a stream object whose dict happens to be `/Type /Sig` with
// `/ByteRange` too (unusual -- signature dictionaries aren't normally
// streams -- but not structurally ruled out by qpdf's own code).
//
// When `refiltered`, `/Filter` and `/DecodeParms` are excluded from the
// entries `visible_dict_entries` ever sees, rather than left in and skipped
// later during the write loop: real qpdf removes those two keys from a
// shallow copy of the dict entirely BEFORE its null-suppression loop even
// starts (`object.removeKey("/Filter")`/`object.removeKey("/DecodeParms")`
// at `QPDFWriter.cc:1454-1455`, both ahead of the shared loop at
// `:1488-1491`) -- it never calls `isNull()` on a key it is about to
// discard anyway. This primitive previously did the opposite order (compute
// suppression over every entry, including `/Filter`/`/DecodeParms`, and
// only skip those two keys afterward inside the write loop), which could
// force-resolve -- and needlessly fail on -- a stale or unsupported
// indirect `/Filter`/`/DecodeParms` reference that is guaranteed to be
// irrelevant to the refiltered output (Codex Review on PR #644,
// crates/flpdf/src/object_handle.rs:2425).
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_stream_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let excluded_entries;
    let entries: &[(Vec<u8>, ObjectHandle)] = if refiltered {
        excluded_entries = entries
            .iter()
            .filter(|entry| {
                entry.0.as_slice() != b"/Filter" && entry.0.as_slice() != b"/DecodeParms"
            })
            .cloned()
            .collect::<Vec<_>>();
        &excluded_entries
    } else {
        entries
    };
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child(value, out)?;
        }
    }
    if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child(length, out)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// QDF-mode sibling of `unparse_stream_dict_entries` above --
// `unparse_stream_body_qdf`'s sole callee -- matching
// `Dictionary::write_pdf_stream_qdf`'s established shape (`object.rs`)
// with `visible_dict_entries`'s null-suppression layered on top, the same
// delegation `unparse_dict_entries_qdf` makes to that helper for the
// plain (non-stream) QDF dictionary case. `/Length` is captured during
// the single suppressed-entries pass and written last, at `indent + 2`,
// immediately before the closing `>>` at `indent` -- no `refiltered`
// dimension exists here, matching `write_pdf_stream_qdf`'s own signature
// (see `unparse_stream_body_qdf`'s own doc for why). Verified byte-for-byte
// against `write_pdf_stream_qdf` (`object.rs`) before this primitive's
// tests were written. Applies the same `/Contents`-in-a-`/Sig`-dictionary
// hex-string special case `unparse_stream_dict_entries` applies, for the
// same reason (see that function's own doc); this function has no
// `refiltered` parameter to begin with, so the Finding-2
// remove-then-suppress reordering that primitive also needed does not apply
// here -- there is no `/Filter`/`/DecodeParms` drop in this function for
// that reordering to fix.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_stream_dict_entries_qdf(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.extend_from_slice(b"<<\n");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2);
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_qdf(value, indent + 2, out)?;
        }
        out.push(b'\n');
    }
    if let Some(length) = length_value {
        push_spaces(out, indent + 2);
        out.extend_from_slice(b"/Length ");
        write_child_qdf(length, indent + 2, out)?;
        out.push(b'\n');
    }
    push_spaces(out, indent);
    out.extend_from_slice(b">>");
    Ok(())
}

fn unparse_stream_dict_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut Vec<u8>,
    map: &ObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    let excluded_entries;
    let entries: &[(Vec<u8>, ObjectHandle)] = if refiltered {
        excluded_entries = entries
            .iter()
            .filter(|entry| {
                entry.0.as_slice() != b"/Filter" && entry.0.as_slice() != b"/DecodeParms"
            })
            .cloned()
            .collect::<Vec<_>>();
        &excluded_entries
    } else {
        entries
    };
    out.extend_from_slice(b"<<");
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            write_child_with_ref_map(value, out, map, removed_refs)?;
        }
    }
    if let Some(length) = length_value {
        out.extend_from_slice(b" /Length ");
        write_child_with_ref_map(length, out, map, removed_refs)?;
    }
    if refiltered {
        out.extend_from_slice(b" /Filter /FlateDecode");
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

impl ObjectHandle {
    /// This trailer-shaped dictionary handle's writer-emission form,
    /// porting the caller-visible shape of `QPDFWriter::writeTrailer`
    /// (`QPDFWriter.cc:1160-1236`): the `"trailer <<"` opener (only when
    /// `xref_stream` is `false` -- the xref-stream dictionary's own `<<`
    /// and xref-specific keys, e.g. `/Type`/`/W`/`/Index`, are the
    /// caller's responsibility, matching `writeXRefStream`'s hand-emitted
    /// literals, `QPDFWriter.cc:2391-2495`, which never route through
    /// `unparseObject` or this primitive at all), an unconditional
    /// per-key loop with no `isNull` suppression (`:1174-1192` has no
    /// such check, unlike `unparseObject`'s dictionary branch that
    /// [`Self::unparse_object`]/[`Self::unparse_object_qdf`]/
    /// [`Self::unparse_stream_body`] all apply through
    /// `visible_dict_entries`), `/ID` and `/Encrypt` excluded from that
    /// loop and forced last in that order when present, and the closing
    /// `>>` (`:1235`, written unconditionally in both `xref_stream`
    /// cases -- this is why `xref_stream = true` still needs a call into
    /// this function at all, despite skipping the opener). Always
    /// produces the compact (non-QDF) one-line form -- `writeTrailer`'s
    /// own `writeStringQDF` calls (`:1169,1175,1190,1195,1233`) are
    /// QDF-only formatting this primitive does not replicate, matching
    /// [`crate::object::Dictionary::write_pdf_trailer`]'s identical
    /// compact-only scope; the QDF classic trailer is emitted separately by
    /// the canonical writer (`write_qdf_trailer`, `writer.rs`).
    ///
    /// **Narrower than the full C++ function -- read before reusing for a
    /// new caller.** Real `writeTrailer` first calls `getTrimmedTrailer()`
    /// (`:1163`, `:2009-2029`) to remove `/ID`, `/Encrypt`, `/Prev`,
    /// `/Index`, `/W`, `/Length`, `/Filter`, `/DecodeParms`, `/Type`, and
    /// `/XRefStm` from a *copy* of the live document trailer before this
    /// shape ever runs; special-cases `/Size`'s *value* from a
    /// `size: int` parameter, with an additional inline `/Prev <offset>`
    /// append when `which == t_lin_first` (`:1179-1186`); and derives
    /// `/ID`'s value from writer state (`generateID()`/`m->id1`/`m->id2`)
    /// and `/Encrypt`'s from `m->encryption_dict_objid` rather than from
    /// the (already-stripped) dict at all. None of that lives here.
    /// Trimming, the `/Size` value substitution, and the `t_lin_first`
    /// inline `/Prev` are the caller's responsibility -- matching this
    /// crate's own already-established split, where
    /// `strip_writer_trailer_history_keys`/`strip_xref_stream_trailer_keys`
    /// (`writer.rs`) do the trimming and `writer.rs:4012`'s
    /// `trailer.insert("Size", ...)` supplies the correct value before
    /// either the legacy `Dictionary::write_pdf_trailer` or this
    /// primitive ever runs. This primitive has no `which`/`size`/`prev`
    /// parameters at all, so `t_lin_first` is out of scope for the same
    /// reason `t_lin_second` is (see below). `/ID` and `/Encrypt` are
    /// read from `self`'s own stored values instead of from writer state
    /// -- the caller is expected to have already placed the correct
    /// values there (`apply_encrypt_trailer_entries`/`generate_id_array`/
    /// `apply_deterministic_id_placeholder`, `writer.rs`), the same contract
    /// [`crate::object::Dictionary::write_pdf_trailer`] already
    /// establishes and this primitive matches for that dimension.
    ///
    /// `id_writer`, when `Some`, substitutes for the stored `/ID` value
    /// (used by the deterministic-`/ID` writer to emit a content-derived
    /// identifier inline). When `None`, the stored `/ID` value is written
    /// in qpdf's compact `[<hex1><hex2>]` shape with no spaces
    /// (mirroring [`crate::object::write_id_style_value`]'s established
    /// byte shape, reimplemented here directly on `ObjectHandle` rather
    /// than bridged through `Object` -- see `write_id_style_value_handle`
    /// below); an indirect `/ID` value writes as its own `"N G R"`
    /// reference form instead, matching `write_child`'s reference-vs-recurse
    /// split rather than being inlined.
    ///
    /// `self` must resolve to a `Dictionary`; a non-dictionary value
    /// (including `self` itself, forced via `try_dereference`, the same
    /// top-level-entry-point pattern [`Self::unparse_object`]/
    /// [`Self::unparse_stream_body`] already use) degrades to an empty
    /// trailer shell, mirroring `write_pdf_stream`/`write_pdf_trailer`'s
    /// own typed-input assumption.
    ///
    /// Out of scope, deliberately: `which == t_lin_second`
    /// (`QPDFWriter.cc:1170-1172`, linearization second pass, `/Size`-only)
    /// and `which == t_lin_first`'s inline `/Prev` (above) have no
    /// equivalent here. A linearization-writer consumer needing either
    /// form is a different primitive.
    #[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
    pub(crate) fn unparse_trailer(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        id_writer: Option<crate::object::TrailerIdWriter>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        self.with_value(|value| {
            let entries: Vec<(Vec<u8>, ObjectHandle)> = match value {
                Some(ObjectValue::Dictionary(entries)) => entries
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };
            unparse_trailer_entries(&entries, xref_stream, id_writer, out)
        })
    }
}

// `unparse_trailer`'s sole callee. Writes the (already-trimmed,
// already-/Size-correct -- see that method's doc) entries in an
// unconditional loop -- no `visible_dict_entries` call, deliberately: this
// is the one dictionary-shaped writer-emission primitive in this family
// that does not suppress null-valued keys, matching `writeTrailer`'s own
// key loop (`QPDFWriter.cc:1174-1192`), which has no `isNull` check
// anywhere in it. Also has no `/Contents`-in-a-`/Sig`-dictionary hex-string
// special case, deliberately: that guard lives in `unparseObject`'s
// dictionary branch alone (`QPDFWriter.cc:1490-1504`), a different loop
// `writeTrailer`'s own key loop never calls into -- `writeTrailer` calls
// `unparseChild(trailer.getKey(key), 1, 0)` directly for every non-`/Size`
// key (`:1188`), and a trailer is never itself a signature dictionary in
// any case.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn unparse_trailer_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    xref_stream: bool,
    mut id_writer: Option<crate::object::TrailerIdWriter>,
    out: &mut Vec<u8>,
) -> Result<()> {
    if !xref_stream {
        out.extend_from_slice(b"trailer <<");
    }
    let mut id_value: Option<&ObjectHandle> = None;
    let mut encrypt_value: Option<&ObjectHandle> = None;
    for (key, value) in entries {
        match key.as_slice() {
            b"/ID" => {
                id_value = Some(value);
                continue;
            }
            b"/Encrypt" => {
                encrypt_value = Some(value);
                continue;
            }
            _ => {}
        }
        out.push(b' ');
        write_dictionary_key(out, key);
        out.push(b' ');
        write_child(value, out)?;
    }
    if let Some(value) = id_value {
        out.extend_from_slice(b" /ID ");
        match id_writer.as_mut() {
            Some(write_id) => write_id(out),
            None => write_id_style_value_handle(value, out)?,
        }
    }
    if let Some(value) = encrypt_value {
        out.extend_from_slice(b" /Encrypt ");
        write_child(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

// Writes a trailer's `/ID` value in qpdf's `writeTrailer` compact shape:
// `[<hex1><hex2>]`, no spaces (`QPDFWriter.cc:1194-1222`, `/ID [` then the
// two identifier strings via `QPDF_String::unparse(true)`, then `]`).
// Mirrors `write_id_style_value` (`object.rs`) byte-for-byte, but walks
// `value`'s own `ObjectHandle` shape directly rather than bridging through
// the legacy `Object` type: an indirect `value` (an `/ID` array stored as
// a reference -- not a shape real qpdf itself ever produces, but nothing
// at the type level rules it out) writes as its own `"N G R"` form via
// `write_child`, checked before any shape inspection, matching
// `write_child`'s own reference-vs-recurse split and never inlining an
// indirect value regardless of what it resolves to. A direct
// `Array([String, String])` gets the compact hex-pair form; any other
// direct shape (wrong arity, non-string elements) falls back to
// `write_child`'s generic form rather than silently truncating -- the
// same "fall back, don't truncate" choice `write_id_style_value` makes.
#[allow(dead_code)] // production callers land when flpdf-egzr.3.2.5 migrates writer consumers onto this API
fn write_id_style_value_handle(value: &ObjectHandle, out: &mut Vec<u8>) -> Result<()> {
    if value.object_ref().is_some() {
        return write_child(value, out);
    }
    let compact: Option<(Vec<u8>, Vec<u8>)> = value.with_value(|v| match v {
        Some(ObjectValue::Array(items)) if items.len() == 2 => {
            let string_bytes = |item: &ObjectHandle| {
                item.with_value(|iv| match iv {
                    Some(ObjectValue::String(s)) => Some(s.clone()),
                    _ => None,
                })
            };
            match (string_bytes(&items[0]), string_bytes(&items[1])) {
                (Some(b0), Some(b1)) => Some((b0, b1)),
                _ => None,
            }
        }
        _ => None,
    });
    match compact {
        Some((b0, b1)) => {
            out.push(b'[');
            crate::object::write_hex_string(out, &b0);
            crate::object::write_hex_string(out, &b1);
            out.push(b']');
            Ok(())
        }
        None => write_child(value, out),
    }
}

// `ObjectHandle::shallow_copy`'s per-variant dispatch: an Array/Dictionary
// child is recursively shallow-copied through `shallow_copy_child` (which
// re-enters `ObjectHandle::shallow_copy`, the recursion hub carrying its
// own `stacker::maybe_grow` wrap — the same hub-per-call shape as
// `unparse_materialize`/`unparse_materialize_child` above), mirroring
// `QPDF_Dictionary::copy`/`QPDF_Array::copy`, which call `shallowCopy` on
// each direct child and keep an indirect one shared. A `Stream` has no
// copy at all: `QPDF_Stream::copy` (`libqpdf/QPDF_Stream.cc:140-145`)
// throws, and it throws from here too — for this value itself and, through
// the recursion, for any direct stream descendant. Every other variant is
// cloned as-is with no further recursion.
fn shallow_copy_value(value: &ObjectValue) -> Result<ObjectValue> {
    Ok(match value {
        ObjectValue::Array(items) => ObjectValue::Array(
            items
                .iter()
                .map(shallow_copy_child)
                .collect::<Result<Vec<_>>>()?,
        ),
        ObjectValue::Dictionary(entries) => ObjectValue::Dictionary(
            entries
                .iter()
                .map(|(k, v)| Ok((k.clone(), shallow_copy_child(v)?)))
                .collect::<Result<std::collections::BTreeMap<_, _>>>()?,
        ),
        ObjectValue::Stream { .. } => {
            return Err(Error::System("stream objects cannot be cloned".to_string()))
        }
        other => other.clone(),
    })
}

fn shallow_copy_child(child: &ObjectHandle) -> Result<ObjectHandle> {
    if child.is_indirect() {
        Ok(child.clone())
    } else {
        child.shallow_copy()
    }
}

// `ObjectHandle::merge_resources`'s per-rtype dictionary merge (the
// `this_val.isDictionary() && other_val.isDictionary()` arm of
// `QPDFObjectHandle::mergeResources`, `libqpdf/QPDFObjectHandle.cc:1095-1129`).
// `this_val` is already the privatized (non-indirect) sub-dictionary by the
// time this is called.
fn merge_resource_subdict(
    this_val: &ObjectHandle,
    other_val: &ObjectHandle,
    rtype: &[u8],
    mut conflicts: Option<&mut ResourceConflicts>,
) -> Result<()> {
    let mut og_to_name: Option<std::collections::HashMap<ObjectRef, Vec<u8>>> = None;
    let mut rnames: std::collections::BTreeSet<Vec<u8>> = std::collections::BTreeSet::new();
    let mut min_suffix: usize = 1;
    let Some(other_sub_entries) = other_val.as_dictionary() else {
        return Ok(()); // cov:ignore: caller already confirmed other_val.as_dictionary().is_some()
    };
    for (key, rval) in other_sub_entries {
        if !this_val.has_key(&key) {
            let installed = if rval.is_indirect() {
                rval
            } else {
                rval.shallow_copy()?
            };
            this_val.replace_key(&key, installed);
            continue;
        }
        let Some(conflicts_map) = conflicts.as_deref_mut() else {
            continue;
        };
        if og_to_name.is_none() {
            og_to_name = Some(build_og_to_name(this_val));
            rnames = get_resource_names(this_val);
        }
        let reused = rval
            .object_ref()
            .and_then(|r| og_to_name.as_ref().and_then(|m| m.get(&r).cloned()));
        if let Some(existing_key) = reused {
            if existing_key != key {
                conflicts_map
                    .entry(rtype.to_vec())
                    .or_default()
                    .insert(key, existing_key);
            }
        } else {
            let new_key = unique_resource_name(&key, &mut min_suffix, &rnames);
            conflicts_map
                .entry(rtype.to_vec())
                .or_default()
                .insert(key, new_key.clone());
            this_val.replace_key(&new_key, rval);
        }
    }
    Ok(())
}

// `ObjectHandle::merge_resources`'s per-rtype array merge (the
// `this_val.isArray() && other_val.isArray()` arm,
// `libqpdf/QPDFObjectHandle.cc:1130-1146`): union `other_val`'s scalar
// items into `this_val` by unparsed text, appending only what is not
// already present.
fn merge_resource_array(this_val: &ObjectHandle, other_val: &ObjectHandle) {
    let Some(other_items) = other_val.as_array() else {
        return; // cov:ignore: caller already confirmed other_val.as_array().is_some()
    };
    let mut scalars: std::collections::BTreeSet<Vec<u8>> = this_val
        .as_array()
        .into_iter()
        .flatten()
        .filter(is_scalar)
        .map(|item| item.unparse())
        .collect();
    for item in other_items {
        if !is_scalar(&item) {
            continue;
        }
        let text = item.unparse();
        if scalars.insert(text) {
            append_array_item(this_val, item);
        }
    }
}

fn append_array_item(handle: &ObjectHandle, item: ObjectHandle) {
    let child = item.clone();
    let appended = handle.with_value_mut(|v| {
        if let Some(ObjectValue::Array(items)) = v {
            items.push(item);
            true
        } else {
            false // cov:ignore: merge_resource_array only calls after confirming this_val is an array
        }
    });
    if appended {
        ObjectHandle::attach_child_to_parent(&child, &handle.containment_parent());
    }
}

// Mirrors `isScalar()` (`libqpdf/QPDFObjectHandle.cc:450-453`): bool,
// integer, name, null, real, or string. Checks only already-resolved/
// direct state, matching every other accessor in this file's "no hidden
// I/O" rule.
fn is_scalar(handle: &ObjectHandle) -> bool {
    handle.as_boolean().is_some()
        || handle.as_integer().is_some()
        || handle.as_name().is_some()
        || handle.is_null()
        || handle.as_real().is_some()
        || handle.as_string().is_some()
}

// Mirrors `mergeResources`'s local `make_og_to_name` lambda
// (`libqpdf/QPDFObjectHandle.cc:1071-1078`): every currently-indirect
// entry in `dict`, keyed by object identity.
fn build_og_to_name(dict: &ObjectHandle) -> std::collections::HashMap<ObjectRef, Vec<u8>> {
    let mut map = std::collections::HashMap::new();
    if let Some(entries) = dict.as_dictionary() {
        for (key, value) in entries {
            if let Some(object_ref) = value.object_ref() {
                map.insert(object_ref, key);
            }
        }
    } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact; the body above is exercised by merge_resources_reuses_an_existing_key_for_the_same_indirect_object
    map
}

// Mirrors `getResourceNames` (`libqpdf/QPDFObjectHandle.cc:1156-1170`,
// `include/qpdf/QPDFObjectHandle.hh:831-835`): the union of every key
// belonging to a dictionary-valued entry of `dict` -- i.e. `dict`'s own
// *grandchildren's* keys, not `dict`'s own keys. See `merge_resources`'s
// own doc comment for why this is the correct level to port here despite
// looking mismatched against its call site.
fn get_resource_names(dict: &ObjectHandle) -> std::collections::BTreeSet<Vec<u8>> {
    let mut result = std::collections::BTreeSet::new();
    if let Some(entries) = dict.as_dictionary() {
        for (_, value) in entries {
            if let Some(sub_entries) = value.as_dictionary() {
                result.extend(sub_entries.into_keys());
            }
        }
    } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact; the body above is exercised by merge_resources_mints_a_second_unique_name_when_the_first_candidate_is_taken
    result
}

// Mirrors `getUniqueResourceName` (`libqpdf/QPDFObjectHandle.cc:1175-1192`):
// append a decimal suffix (starting at `*min_suffix`) to `key` + `"_"`
// until the result is absent from `names`, leaving `*min_suffix` at the
// value just used (not incremented past it -- a caller minting several
// names in the same sub-dictionary reuses the search position, matching
// qpdf's own "used, not next" contract).
fn unique_resource_name(
    key: &[u8],
    min_suffix: &mut usize,
    names: &std::collections::BTreeSet<Vec<u8>>,
) -> Vec<u8> {
    let mut prefix = key.to_vec();
    prefix.push(b'_');
    let max_suffix = *min_suffix + names.len();
    while *min_suffix <= max_suffix {
        let mut candidate = prefix.clone();
        candidate.extend(min_suffix.to_string().into_bytes());
        if !names.contains(&candidate) {
            return candidate;
        }
        *min_suffix += 1;
    }
    // Unreachable per qpdf's own invariant: this loop tests strictly more
    // candidates (names.len() + 1) than there are names to conflict with,
    // so by pigeonhole one must be free. qpdf itself treats reaching this
    // point as a coding error (throws std::logic_error); this crate has no
    // exception channel to mirror that with, so panic the same way an
    // internal invariant violation elsewhere in this crate would.
    unreachable!("no unconflicting resource name found") // cov:ignore: unreachable, see comment above
}

// True if `handle`'s value is already known (no resolution performed here)
// to be null: a direct null, or an indirect handle that is already resolved
// (`ObjectHandle::is_resolved`) and reads as null. qpdf's own check
// (`QPDFObjectHandle::isNull()`, `libqpdf/QPDFObjectHandle.cc:353-356`)
// dereferences an indirect child to decide this; this port never performs
// that hidden resolution (matching every other accessor in this file), so a
// not-yet-resolved indirect entry is conservatively treated as *not* known
// to be null and is kept rather than guessed away.
fn unparse_is_known_null(handle: &ObjectHandle) -> bool {
    handle.is_resolved() && handle.is_null()
}

// Tears down a materialized `Object` tree without using its own recursive
// Drop glue, which -- like `unparse_materialize`'s construction walk this
// mirrors -- has no protection against a deeply nested tree's per-frame
// stack cost. Takes ownership so the caller's normal drop of the same value
// never runs (its children have already been moved out and pushed onto this
// function's own explicit, heap-allocated stack by the time each node's
// turn to drop trivially arrives).
//
// Only `Array`/`Dictionary`/`Stream` nest another `Object`; every other
// variant holds no `Object` children and drops in O(1) once popped, whether
// or not this function ever visits it -- `Dictionary`/`Stream` are drained
// through their existing public `iter()`/`remove()` API (no new access
// needed into `object.rs`, kept outside this slice's file allowlist).
fn unparse_drop_iteratively(root: Object) {
    let mut stack = vec![root];
    while let Some(mut node) = stack.pop() {
        match &mut node {
            Object::Array(items) => stack.extend(std::mem::take(items)),
            Object::Dictionary(dict) => drain_dictionary_onto(dict, &mut stack),
            Object::Stream(stream) => drain_dictionary_onto(&mut stream.dict, &mut stack),
            _ => {}
        }
        // `node`'s own nested `Object` children (if any) were just moved
        // out above, so its normal drop here -- an empty `Vec`/`Dictionary`
        // plus whatever non-recursive fields it holds -- is O(1).
    }
}

fn drain_dictionary_onto(dict: &mut Dictionary, stack: &mut Vec<Object>) {
    let keys: Vec<Vec<u8>> = dict.iter().map(|(key, _)| key.to_vec()).collect();
    for key in keys {
        if let Some(value) = dict.remove(key) {
            stack.push(value);
        }
    }
}

#[cfg(test)]
pub(crate) mod identity_tests {
    use super::*;

    struct RecordingResolver {
        calls: ResolutionLog,
        value: ObjectValue,
    }

    /// Every `resolve_indirect` a [`RecordingResolver`] performed, in order.
    ///
    /// Shared with the caller by [`logged_resolver_bearing_handle`] so a test
    /// can assert a *negative*: that a position was never resolved at all.
    /// `ObjectHandle::is_resolved` is not a substitute — a resolver that
    /// errored would leave the handle unresolved despite having been called.
    pub(crate) type ResolutionLog = Rc<RefCell<Vec<ObjectRef>>>;

    impl RecordingResolver {
        /// Install `value` instead of the default one-key dictionary, so a
        /// test can exercise a resolving accessor for a non-dictionary shape.
        ///
        /// One instance installs the *same* child handles on every resolution:
        /// cloning an `ObjectValue` container clones child `Rc`s rather than
        /// the subtree (see that enum's own doc). Resolving two handles through
        /// a single resolver therefore leaves their children `ptr_eq`, which no
        /// current test wants — give each such test its own resolver.
        fn installing(value: ObjectValue) -> Self {
            Self::logging_into(Rc::new(RefCell::new(Vec::new())), value)
        }

        /// [`Self::installing`] with the call log owned by the caller instead.
        fn logging_into(calls: ResolutionLog, value: ObjectValue) -> Self {
            Self { calls, value }
        }
    }

    impl Default for RecordingResolver {
        fn default() -> Self {
            Self::installing(ObjectValue::Dictionary(
                [(b"A".to_vec(), ObjectHandle::integer(1))]
                    .into_iter()
                    .collect(),
            ))
        }
    }

    impl DocumentResolver for RecordingResolver {
        fn resolve_indirect(
            &self,
            object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            self.calls.borrow_mut().push(object_ref);
            handle.set_resolved(self.value.clone());
            Ok(())
        }
    }

    /// Resolves a stream value but intentionally has no byte source. This
    /// exercises `DocumentResolver::pipe_stream_data`'s default boundary: a
    /// resolver may resolve objects without also being a file-backed reader.
    struct NoStreamSourceResolver;

    impl DocumentResolver for NoStreamSourceResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            handle.set_resolved(ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![]),
                stream_data: None,
                stream_provider: None,
                stream_length: 3,
            });
            Ok(())
        }
    }

    struct NonStreamDestinationResolver;

    impl DocumentResolver for NonStreamDestinationResolver {
        fn resolve_indirect(
            &self,
            object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            NoStreamSourceResolver.resolve_indirect(object_ref, handle)
        }

        fn new_stream(&self) -> crate::Result<ObjectHandle> {
            Ok(ObjectHandle::integer(1))
        }
    }

    /// An unresolved indirect handle whose resolver installs `value`.
    ///
    /// `pub(crate)` so `stream_filter.rs`'s handle-shape reader tests can
    /// build an indirect child without a second harness; the returned
    /// resolver is erased, so `RecordingResolver` itself stays private here.
    ///
    /// **The caller must keep the returned resolver alive**, and bind it to a
    /// named `_resolver` rather than to `_` — the latter drops it immediately.
    /// The handle holds only a `Weak`, so a dropped resolver turns every
    /// accessor into `Error::Internal("object 20 0 belongs to a dropped
    /// PDF")`: a test expecting a resolved value then fails confusingly, and
    /// one expecting an error passes for the wrong reason. Dropping it
    /// *deliberately* is how to build a dropped-document handle, as
    /// `handle_reader_surfaces_a_dropped_document_from_every_child_position`
    /// does.
    pub(crate) fn resolver_bearing_handle(
        value: ObjectValue,
    ) -> (ObjectHandle, Rc<dyn DocumentResolver>) {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::installing(value));
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver),
        );
        // The resolver is returned so the caller keeps it alive: the handle
        // holds only a `Weak`, and dropping it here would turn every accessor
        // into the dropped-document error instead.
        (handle, resolver)
    }

    #[test]
    fn raw_stream_data_requires_a_file_backed_resolver_for_original_bytes() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(NoStreamSourceResolver);
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver),
        );
        stream.set_parsed_offset_if_unset(9);

        let error = stream
            .get_raw_stream_data()
            .expect_err("an object-only resolver has no original stream source");
        assert!(matches!(error, Error::Internal(message)
            if message == "stream data requested from a resolver without a stream source"));
    }

    #[test]
    fn stream_copy_resolver_defaults_report_missing_document_operations() {
        let resolver = NoStreamSourceResolver;
        assert!(matches!(
            resolver.new_stream(),
            Err(Error::Internal(message))
                if message == "stream creation requested from a resolver without a document"
        ));
        assert!(matches!(
            resolver.copy_stream_data(&ObjectHandle::null(), &ObjectHandle::null()),
            Err(Error::Internal(message))
                if message == "stream data copy requested from a resolver without a document"
        ));
        assert!(!resolver.immediate_copy_from());
    }

    #[test]
    fn copy_stream_rejects_a_non_stream_created_by_its_resolver() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(NonStreamDestinationResolver);
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver),
        );
        let error = stream
            .copy_stream()
            .expect_err("resolver returned a non-stream destination");
        assert!(matches!(error, Error::Internal(message)
            if message == "copyStream created a non-stream destination"));
    }

    /// [`resolver_bearing_handle`] plus the resolver's [`ResolutionLog`].
    ///
    /// For the one question the plain helper cannot answer: whether a child
    /// position was resolved *at all*. The same "keep the resolver alive"
    /// rule applies — an empty log proves nothing if the resolver was
    /// dropped, since a severed handle never reaches `resolve_indirect`
    /// either.
    pub(crate) fn logged_resolver_bearing_handle(
        value: ObjectValue,
    ) -> (ObjectHandle, Rc<dyn DocumentResolver>, ResolutionLog) {
        let calls: ResolutionLog = Rc::new(RefCell::new(Vec::new()));
        let resolver: Rc<dyn DocumentResolver> =
            Rc::new(RecordingResolver::logging_into(Rc::clone(&calls), value));
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver),
        );
        (handle, resolver, calls)
    }

    struct MissingResolver;

    impl DocumentResolver for MissingResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            handle.set_missing();
            Ok(())
        }
    }

    struct ErrorResolver;

    impl DocumentResolver for ErrorResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            _handle: &ObjectHandle,
        ) -> crate::Result<()> {
            Err(Error::System("resolver failed".to_string()))
        }
    }

    /// Records a resolution attempt before returning its configured error.
    ///
    /// Unlike [`ErrorResolver`], each instance has an independently chosen
    /// message and externally visible call log. This lets an enumeration test
    /// distinguish qpdf's lexical fail-fast traversal from a reversed walk
    /// that happens to return an error of the same variant.
    struct LoggedErrorResolver {
        calls: ResolutionLog,
        message: String,
    }

    impl DocumentResolver for LoggedErrorResolver {
        fn resolve_indirect(
            &self,
            object_ref: ObjectRef,
            _handle: &ObjectHandle,
        ) -> crate::Result<()> {
            self.calls.borrow_mut().push(object_ref);
            Err(Error::System(self.message.clone()))
        }
    }

    /// An unresolved indirect handle whose resolver always errors, mirroring
    /// [`resolver_bearing_handle`]'s own doc but for a position that must
    /// never be resolved at all rather than one that resolves to a
    /// particular value -- a test asserting some code path does not resolve
    /// a value it does not need can build one of these and assert success:
    /// actually resolving it here would surface as an error instead.
    ///
    /// `pub(crate)` for the same reason as [`resolver_bearing_handle`]: a
    /// harness other test modules in this file need, not just this one.
    /// Same "keep the resolver alive" rule applies -- see that function's
    /// own doc.
    pub(crate) fn error_resolving_handle(
        object_ref: ObjectRef,
    ) -> (ObjectHandle, Rc<dyn DocumentResolver>) {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(ErrorResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(object_ref, Rc::downgrade(&resolver));
        (handle, resolver)
    }

    fn logged_error_resolving_handle(
        object_ref: ObjectRef,
        message: impl Into<String>,
    ) -> (ObjectHandle, Rc<dyn DocumentResolver>, ResolutionLog) {
        let calls: ResolutionLog = Rc::new(RefCell::new(Vec::new()));
        let resolver: Rc<dyn DocumentResolver> = Rc::new(LoggedErrorResolver {
            calls: Rc::clone(&calls),
            message: message.into(),
        });
        let handle = ObjectHandle::new_indirect_with_resolver(object_ref, Rc::downgrade(&resolver));
        (handle, resolver, calls)
    }

    #[test]
    fn try_get_key_resolves_the_same_indirect_slot_once() {
        let resolver = Rc::new(RecordingResolver::default());
        let erased: Rc<dyn DocumentResolver> = resolver.clone();
        let handle =
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(7, 0), Rc::downgrade(&erased));
        let clone = handle.clone();

        assert_eq!(handle.try_get_key(b"/A").unwrap().as_integer(), Some(1));
        assert!(clone.try_has_key(b"/A").unwrap());
        assert_eq!(*resolver.calls.borrow(), vec![ObjectRef::new(7, 0)]);
        assert!(handle.ptr_eq(&clone));
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(7, 0)));
    }

    #[test]
    fn try_dereference_reports_a_dropped_document_without_reconnecting() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(8, 0),
            Rc::downgrade(&resolver),
        );
        drop(resolver);

        let error = handle.try_dereference().unwrap_err();
        assert_eq!(error.to_string(), "object 8 0 belongs to a dropped PDF");
        assert!(!handle.is_resolved());
    }

    /// A handle needs its owning document's identity *and* that document's
    /// resolver at once: `Pdf::get_object_handle` hands out one handle that
    /// must answer both questions.
    ///
    /// The identity is not decorative. `set_resolved` propagates the slot's
    /// `pdf_unique_id` through every current direct descendant independently
    /// of the live immediate-parent edges. The identity provenance drives
    /// [`ObjectHandle::belongs_to_pdf`], while those edges drive
    /// [`ObjectHandle::containing_object_refs_for_pdf`] — respectively the
    /// foreign-object rejection and current owner lookup in
    /// `Pdf::mark_object_handle_dirty`, `filespec_helper`, and
    /// `embedded_files`. Measured against the current tree, not predicted:
    /// this is now the constructor `Pdf::get_object_handle` uses — the
    /// identity-only `new_indirect_unresolved_for_pdf` was deleted when it
    /// switched over — and patching it to discard its `pdf_unique_id`
    /// argument fails 62 tests in `cargo test -p flpdf --lib`, not just this
    /// one.
    ///
    /// Note this is *not* what `Pdf::is_canonical_object_handle` compares on:
    /// that one looks the ref up in `handle_registry` and compares `Rc`
    /// pointers, never touching `pdf_unique_id`.
    #[test]
    fn an_indirect_slot_carries_both_its_pdf_identity_and_its_resolver() {
        const PDF_ID: u64 = 4242;
        let object_ref = ObjectRef::new(13, 0);
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
            object_ref,
            NO_PARSED_OFFSET,
            PDF_ID,
            Rc::downgrade(&resolver),
        );

        // Identity: preserved, and specific to this document rather than
        // matching any id put to it.
        assert!(handle.belongs_to_pdf(PDF_ID));
        assert!(!handle.belongs_to_pdf(PDF_ID + 1));

        // Resolver: reachable through `try_dereference`'s real path — upgrade
        // the `Weak`, call `resolve_indirect` — not merely stored in the slot.
        // Without it this is the dropped-document error instead.
        handle.try_dereference().unwrap();
        assert!(handle.is_resolved());

        // Both at once. The child's identity and live root edge are written
        // by `set_resolved`, so both can only be present if the document
        // identity survived *into* the resolution the resolver drove.
        let child = handle.get_key(b"/A");
        assert_eq!(child.as_integer(), Some(1));
        assert_eq!(
            child.containing_object_refs_for_pdf(PDF_ID),
            vec![object_ref]
        );
        assert!(child.containing_object_refs_for_pdf(PDF_ID + 1).is_empty());
    }

    #[test]
    fn canonical_payload_sharing_rejects_invalid_sources_without_mutating() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(30, 0), -1);
        let direct = ObjectHandle::integer(1);
        direct
            .share_value_state_with(&direct)
            .expect("sharing a handle with itself is already a no-op");

        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(31, 0), -1);
        let error = target
            .share_value_state_with(&indirect)
            .expect_err("an indirect replacement is not a payload source");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: replacement ObjectHandle must be direct"
        );

        let destroyed = ObjectHandle::integer(2);
        let destroyed_resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        destroyed.promote_to_indirect(
            ObjectRef::new(32, 0),
            4242,
            Rc::downgrade(&destroyed_resolver),
        );
        destroyed.disconnect();
        let error = target
            .share_value_state_with(&destroyed)
            .expect_err("a destroyed direct payload is not initialized");
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: replacement ObjectHandle is not initialized"
        );
        assert!(!target.is_resolved());
    }

    #[test]
    fn removing_a_direct_handle_is_a_no_op() {
        let direct = ObjectHandle::integer(1);
        direct.remove_from_document();
        assert_eq!(direct.as_integer(), Some(1));
    }

    #[test]
    fn exclusive_pdf_ownership_walk_handles_cycles_and_indirect_mismatch() {
        let first = ObjectHandle::dictionary(vec![]);
        let second = ObjectHandle::dictionary(vec![]);
        first.replace_key(b"/Second", second.clone());
        second.replace_key(b"/First", first.clone());
        assert!(first.belongs_exclusively_to_pdf(4242));

        let foreign_resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let foreign_indirect = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(33, 0),
            NO_PARSED_OFFSET,
            4243,
            Rc::downgrade(&foreign_resolver),
        );
        assert!(!foreign_indirect.belongs_exclusively_to_pdf(4242));

        let promoted_foreign = ObjectHandle::integer(3);
        promoted_foreign.promote_to_indirect(
            ObjectRef::new(34, 0),
            4243,
            Rc::downgrade(&foreign_resolver),
        );
        assert_eq!(promoted_foreign.object_ref(), Some(ObjectRef::new(34, 0)));
        assert_eq!(promoted_foreign.0.borrow().active_pdf_unique_id, Some(4243));
        assert!(!promoted_foreign.belongs_exclusively_to_pdf(4242));
    }

    #[test]
    fn exclusive_pdf_ownership_walk_checks_all_indirect_children() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let same_document = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(35, 0),
            NO_PARSED_OFFSET,
            4242,
            Rc::downgrade(&resolver),
        );
        let foreign_document = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(36, 0),
            NO_PARSED_OFFSET,
            4243,
            Rc::downgrade(&resolver),
        );

        // The LIFO walk visits the same-document child first in this order;
        // it must still inspect the foreign sibling before accepting the
        // container.
        let same_first = ObjectHandle::array(vec![foreign_document.clone(), same_document.clone()]);
        assert!(!same_first.belongs_exclusively_to_pdf(4242));

        let foreign_first = ObjectHandle::array(vec![same_document, foreign_document]);
        assert!(!foreign_first.belongs_exclusively_to_pdf(4242));
    }

    #[test]
    fn removing_shared_canonical_state_rebinds_only_the_canonical_slot() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(37, 0), -1);
        let replacement =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]);
        target
            .share_value_state_with(&replacement)
            .expect("share replacement payload");

        target.remove_from_document();

        assert!(target.is_direct());
        assert!(target.is_null());
        assert_eq!(replacement.get_key(b"/Value").as_integer(), Some(7));
        replacement.replace_key(b"/Value", ObjectHandle::integer(9));
        assert_eq!(replacement.get_key(b"/Value").as_integer(), Some(9));
    }

    #[test]
    fn disconnecting_shared_canonical_state_rebinds_only_the_canonical_slot() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(38, 0), -1);
        let replacement =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(7))]);
        target
            .share_value_state_with(&replacement)
            .expect("share replacement payload");

        target.disconnect();

        assert_eq!(target.type_code(), 14);
        assert_eq!(replacement.get_key(b"/Value").as_integer(), Some(7));
    }

    #[test]
    fn shared_state_prunes_dropped_owners() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(39, 0), -1);
        let source = ObjectHandle::integer(7);
        target
            .share_value_state_with(&source)
            .expect("share replacement payload");

        drop(target);
        source.replace_direct_value(ObjectValue::Integer(8));

        assert_eq!(source.as_integer(), Some(8));
    }

    #[test]
    fn detached_child_preserves_pdf_identity_without_a_live_root() {
        let owner_ref = ObjectRef::new(7, 0);
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let owner = ObjectHandle::new_indirect_for_pdf_with_resolver(
            owner_ref,
            NO_PARSED_OFFSET,
            41,
            Rc::downgrade(&resolver),
        );
        let parent = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Parent".to_vec(), parent.clone())].into_iter().collect(),
        ));
        let child = ObjectHandle::dictionary(vec![]);
        parent.replace_key(b"/Child", child.clone());

        parent.remove_key(b"/Child");

        assert!(child.belongs_to_pdf(41));
        assert!(!child.belongs_to_pdf(42));
        assert!(child.containing_object_refs_for_pdf(41).is_empty());
    }

    #[test]
    fn pdf_identity_propagation_terminates_on_a_direct_cycle() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let owner = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(7, 0),
            NO_PARSED_OFFSET,
            41,
            Rc::downgrade(&resolver),
        );
        let first = ObjectHandle::dictionary(vec![]);
        let second = ObjectHandle::dictionary(vec![]);
        first.replace_key(b"/Second", second.clone());
        second.replace_key(b"/First", first.clone());

        owner.set_resolved(ObjectValue::Dictionary(
            [(b"First".to_vec(), first.clone())].into_iter().collect(),
        ));

        assert!(first.belongs_to_pdf(41));
        assert!(second.belongs_to_pdf(41));
    }

    #[test]
    fn resolver_bearing_indirect_slot_starts_without_a_parsed_offset() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(12, 0),
            Rc::downgrade(&resolver),
        );

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn missing_indirect_slot_resolves_in_place_to_null() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(MissingResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(9, 0),
            Rc::downgrade(&resolver),
        );

        assert!(handle.try_is_null().unwrap());
        assert!(handle.is_resolved());
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(9, 0)));
    }

    #[test]
    fn every_fallible_accessor_propagates_the_resolver_error() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(ErrorResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(10, 0),
            Rc::downgrade(&resolver),
        );

        assert_eq!(
            handle.try_is_null().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_dictionary().unwrap_err().to_string(),
            "resolver failed"
        );
        assert!(
            matches!(handle.try_get_keys().unwrap_err(), Error::System(message)
            if message == "resolver failed")
        );
        assert_eq!(
            handle.try_get_key(b"/A").unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_has_key(b"/A").unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_name().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_array().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_array_len().unwrap_err().to_string(),
            "resolver failed"
        );
        assert_eq!(
            handle.try_as_integer().unwrap_err().to_string(),
            "resolver failed"
        );
        assert!(!handle.is_resolved());
    }

    #[test]
    fn try_get_keys_resolves_every_value_omits_nullish_and_sorts_keys() {
        let (indirect_null, _indirect_null_resolver) = resolver_bearing_handle(ObjectValue::Null);

        let missing_resolver: Rc<dyn DocumentResolver> = Rc::new(MissingResolver);
        let missing = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(21, 0),
            Rc::downgrade(&missing_resolver),
        );

        let (unknown, _unknown_resolver, unknown_calls) =
            logged_resolver_bearing_handle(ObjectValue::Integer(2));

        let dict = ObjectHandle::dictionary(vec![
            (b"Zulu".to_vec(), ObjectHandle::integer(1)),
            (b"DirectNull".to_vec(), ObjectHandle::null()),
            (b"IndirectNull".to_vec(), indirect_null.clone()),
            (b"Dangling".to_vec(), missing.clone()),
            (b"Unknown".to_vec(), unknown),
            (b"Alpha".to_vec(), ObjectHandle::boolean(true)),
        ]);

        assert_eq!(
            dict.try_get_keys().unwrap(),
            BTreeSet::from([b"/Alpha".to_vec(), b"/Unknown".to_vec(), b"/Zulu".to_vec(),])
        );
        assert!(indirect_null.is_resolved());
        assert!(missing.is_resolved());
        assert_eq!(*unknown_calls.borrow(), vec![ObjectRef::new(20, 0)]);
    }

    #[test]
    fn try_get_keys_lazily_resolves_dictionary_and_non_dictionary_holders() {
        let (dict, _dict_resolver, dict_calls) =
            logged_resolver_bearing_handle(ObjectValue::Dictionary(
                [(b"Keep".to_vec(), ObjectHandle::integer(1))]
                    .into_iter()
                    .collect(),
            ));
        assert!(!dict.is_resolved());
        assert_eq!(
            dict.try_get_keys().unwrap(),
            BTreeSet::from([b"/Keep".to_vec()])
        );
        assert!(dict.is_resolved());
        assert_eq!(*dict_calls.borrow(), vec![ObjectRef::new(20, 0)]);

        let (scalar, _scalar_resolver, scalar_calls) =
            logged_resolver_bearing_handle(ObjectValue::Integer(7));
        assert!(matches!(
            scalar.try_get_keys().unwrap_err(),
            Error::Internal(message)
                if message == "warning raised through a resolver with no document warning sink: \
                    object 20 0: operation for dictionary attempted on object of type integer: \
                    treating as empty"
        ));
        assert_eq!(*scalar_calls.borrow(), vec![ObjectRef::new(20, 0)]);
    }

    #[test]
    fn try_get_keys_propagates_a_child_resolver_error() {
        let (child, _resolver) = error_resolving_handle(ObjectRef::new(30, 0));
        let dict = ObjectHandle::dictionary(vec![(b"Broken".to_vec(), child.clone())]);

        assert!(
            matches!(dict.try_get_keys().unwrap_err(), Error::System(message)
            if message == "resolver failed")
        );
        assert!(!child.is_resolved());
    }

    #[test]
    fn try_get_keys_propagates_a_dropped_resolver_error_from_holder_and_child() {
        let (holder, holder_resolver) =
            resolver_bearing_handle(ObjectValue::Dictionary(Default::default()));
        drop(holder_resolver);
        assert!(
            matches!(holder.try_get_keys().unwrap_err(), Error::Internal(message)
            if message == "object 20 0 belongs to a dropped PDF")
        );

        let (child, child_resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(child_resolver);
        let dict = ObjectHandle::dictionary(vec![(b"Broken".to_vec(), child)]);
        assert!(
            matches!(dict.try_get_keys().unwrap_err(), Error::Internal(message)
            if message == "object 20 0 belongs to a dropped PDF")
        );
    }

    #[test]
    fn try_get_keys_stops_at_the_lexical_first_child_resolver_error() {
        let (zulu, _zulu_resolver, zulu_calls) =
            logged_error_resolving_handle(ObjectRef::new(32, 0), "zulu resolver failed");
        let (alpha, _alpha_resolver, alpha_calls) =
            logged_error_resolving_handle(ObjectRef::new(31, 0), "alpha resolver failed");
        // Insert in reverse lexical order: the dictionary's BTreeMap traversal
        // must nevertheless resolve Alpha first and stop there.
        let dict =
            ObjectHandle::dictionary(vec![(b"Zulu".to_vec(), zulu), (b"Alpha".to_vec(), alpha)]);

        assert!(
            matches!(dict.try_get_keys().unwrap_err(), Error::System(message)
            if message == "alpha resolver failed")
        );
        assert_eq!(*alpha_calls.borrow(), vec![ObjectRef::new(31, 0)]);
        assert!(zulu_calls.borrow().is_empty());
    }

    #[test]
    fn try_has_key_treats_a_present_null_value_as_absent() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);

        assert!(!dict.try_has_key(b"/A").unwrap());
        assert!(!dict.try_has_key(b"/Missing").unwrap());
    }

    #[test]
    fn fallible_dictionary_accessors_cover_resolved_and_non_dictionary_values() {
        let resolver = Rc::new(RecordingResolver::default());
        let erased: Rc<dyn DocumentResolver> = resolver;
        let dict =
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(11, 0), Rc::downgrade(&erased));

        let entries = dict
            .try_as_dictionary()
            .unwrap()
            .expect("recording resolver installs a dictionary");
        assert_eq!(entries.get(b"/A".as_slice()).unwrap().as_integer(), Some(1));

        let scalar = ObjectHandle::integer(1);
        assert!(!scalar.try_has_key(b"/A").unwrap());
    }

    #[test]
    fn try_as_name_resolves_an_indirect_name_through_its_document() {
        let (handle, _resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"FlateDecode".to_vec()));

        // The non-resolving accessor cannot see through an unresolved handle,
        // and reports the same `None` it would for a wrong-typed value. Closing
        // that gap is the whole reason the `try_` form exists.
        assert_eq!(handle.as_name(), None);
        assert!(!handle.is_resolved());

        assert_eq!(handle.try_as_name().unwrap(), Some(b"FlateDecode".to_vec()));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_is_name_and_equals_compares_direct_decoded_name_bytes() {
        let name = ObjectHandle::name(b"Crypt".to_vec());

        assert!(name.try_is_name_and_equals(b"Crypt").unwrap());
        assert!(!name.try_is_name_and_equals(b"FlateDecode").unwrap());
        assert!(!ObjectHandle::integer(1)
            .try_is_name_and_equals(b"Crypt")
            .unwrap());
    }

    #[test]
    fn try_is_name_and_equals_resolves_an_indirect_name() {
        let (handle, _resolver) = resolver_bearing_handle(ObjectValue::Name(b"Crypt".to_vec()));

        assert!(!handle.is_resolved());
        assert!(handle.try_is_name_and_equals(b"Crypt").unwrap());
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_is_name_and_equals_propagates_resolver_errors() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(ErrorResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(22, 0),
            Rc::downgrade(&resolver),
        );

        assert_eq!(
            handle
                .try_is_name_and_equals(b"Crypt")
                .unwrap_err()
                .to_string(),
            "resolver failed"
        );
    }

    #[test]
    fn try_is_name_and_equals_reports_a_dropped_document() {
        let (handle, resolver) = resolver_bearing_handle(ObjectValue::Name(b"Crypt".to_vec()));
        drop(resolver);

        assert_eq!(
            handle
                .try_is_name_and_equals(b"Crypt")
                .unwrap_err()
                .to_string(),
            "object 20 0 belongs to a dropped PDF"
        );
    }

    #[test]
    fn try_is_or_has_name_matches_a_direct_name_or_array_item() {
        assert!(ObjectHandle::name(b"Crypt".to_vec())
            .try_is_or_has_name(b"Crypt")
            .unwrap());
        assert!(ObjectHandle::array(vec![
            ObjectHandle::name(b"FlateDecode".to_vec()),
            ObjectHandle::name(b"Crypt".to_vec()),
        ])
        .try_is_or_has_name(b"Crypt")
        .unwrap());
    }

    #[test]
    fn try_is_or_has_name_returns_false_for_other_shapes_and_names() {
        assert!(!ObjectHandle::name(b"FlateDecode".to_vec())
            .try_is_or_has_name(b"Crypt")
            .unwrap());
        assert!(!ObjectHandle::integer(1)
            .try_is_or_has_name(b"Crypt")
            .unwrap());
        assert!(!ObjectHandle::array(vec![ObjectHandle::integer(1)])
            .try_is_or_has_name(b"Crypt")
            .unwrap());
    }

    #[test]
    fn try_is_or_has_name_stops_before_a_later_erroring_child() {
        let (erroring, _resolver) = error_resolving_handle(ObjectRef::new(24, 0));
        let array = ObjectHandle::array(vec![ObjectHandle::name(b"Crypt".to_vec()), erroring]);

        assert!(array.try_is_or_has_name(b"Crypt").unwrap());
    }

    #[test]
    fn try_is_or_has_name_resolves_indirect_holder_and_child() {
        let (child, _child_resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"Crypt".to_vec()));
        let (array, _array_resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![child.clone()]));

        assert!(array.try_is_or_has_name(b"Crypt").unwrap());
        assert!(array.is_resolved());
        assert!(child.is_resolved());
    }

    #[test]
    fn try_is_or_has_name_propagates_child_resolution_errors() {
        let (erroring, _resolver) = error_resolving_handle(ObjectRef::new(25, 0));
        let array = ObjectHandle::array(vec![erroring]);

        assert_eq!(
            array.try_is_or_has_name(b"Crypt").unwrap_err().to_string(),
            "resolver failed"
        );
    }

    #[test]
    fn try_is_or_has_name_reports_a_dropped_document() {
        let (array, resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![ObjectHandle::null()]));
        drop(resolver);

        assert_eq!(
            array.try_is_or_has_name(b"Crypt").unwrap_err().to_string(),
            "object 20 0 belongs to a dropped PDF"
        );
    }

    #[test]
    fn try_is_dictionary_of_type_matches_type_subtype_and_empty_constraints() {
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Type".to_vec(),
                ObjectHandle::name(b"CryptFilterDecodeParms".to_vec()),
            ),
            (
                b"Subtype".to_vec(),
                ObjectHandle::name(b"Identity".to_vec()),
            ),
        ]);

        assert!(dict
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")
            .unwrap());
        assert!(dict
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"Identity")
            .unwrap());
        assert!(dict.try_is_dictionary_of_type(b"", b"").unwrap());
        assert!(dict.try_is_dictionary_of_type(b"", b"Identity").unwrap());
    }

    #[test]
    fn try_is_dictionary_of_type_rejects_wrong_missing_or_non_name_entries() {
        let wrong = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"Metadata".to_vec()),
        )]);
        let non_name = ObjectHandle::dictionary(vec![(b"Type".to_vec(), ObjectHandle::integer(1))]);
        let missing = ObjectHandle::dictionary(Vec::new());
        let wrong_subtype = ObjectHandle::dictionary(vec![
            (
                b"Type".to_vec(),
                ObjectHandle::name(b"CryptFilterDecodeParms".to_vec()),
            ),
            (b"Subtype".to_vec(), ObjectHandle::name(b"Other".to_vec())),
        ]);

        assert!(!wrong
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")
            .unwrap());
        assert!(!non_name
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")
            .unwrap());
        assert!(!missing
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")
            .unwrap());
        assert!(!wrong_subtype
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"Identity")
            .unwrap());
        assert!(!ObjectHandle::integer(1)
            .try_is_dictionary_of_type(b"", b"")
            .unwrap());
    }

    #[test]
    fn try_is_dictionary_of_type_resolves_indirect_holder_and_type_child() {
        let (type_name, _type_resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"CryptFilterDecodeParms".to_vec()));
        let (dict, _dict_resolver) =
            resolver_bearing_handle(ObjectValue::Dictionary(std::collections::BTreeMap::from([
                (b"Type".to_vec(), type_name.clone()),
            ])));

        assert!(dict
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")
            .unwrap());
        assert!(dict.is_resolved());
        assert!(type_name.is_resolved());
    }

    #[test]
    fn try_is_dictionary_of_type_stops_before_subtype_after_wrong_type() {
        let (erroring_subtype, _resolver) = error_resolving_handle(ObjectRef::new(26, 0));
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Metadata".to_vec())),
            (b"Subtype".to_vec(), erroring_subtype),
        ]);

        assert!(!dict
            .try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"Identity")
            .unwrap());
    }

    #[test]
    fn try_is_dictionary_of_type_propagates_child_resolution_errors() {
        let (erroring_type, _resolver) = error_resolving_handle(ObjectRef::new(27, 0));
        let dict = ObjectHandle::dictionary(vec![(b"Type".to_vec(), erroring_type)]);

        assert_eq!(
            dict.try_is_dictionary_of_type(b"CryptFilterDecodeParms", b"")
                .unwrap_err()
                .to_string(),
            "resolver failed"
        );
    }

    #[test]
    fn try_is_dictionary_of_type_reports_a_dropped_document() {
        let (dict, resolver) =
            resolver_bearing_handle(ObjectValue::Dictionary(std::collections::BTreeMap::new()));
        drop(resolver);

        assert_eq!(
            dict.try_is_dictionary_of_type(b"", b"")
                .unwrap_err()
                .to_string(),
            "object 20 0 belongs to a dropped PDF"
        );
    }

    #[test]
    fn try_as_integer_resolves_an_indirect_integer_through_its_document() {
        let (handle, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));

        assert_eq!(handle.as_integer(), None);
        assert!(!handle.is_resolved());

        assert_eq!(handle.try_as_integer().unwrap(), Some(7));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_as_array_resolves_an_indirect_array_through_its_document() {
        let (handle, _resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![ObjectHandle::from_value(
                ObjectValue::Name(b"FlateDecode".to_vec()),
            )]));

        assert!(handle.as_array().is_none());
        assert!(!handle.is_resolved());

        let items = handle
            .try_as_array()
            .unwrap()
            .expect("recording resolver installs an array");
        // `ObjectHandle` equality is identity rather than value (see
        // `two_direct_handles_with_equal_value_are_distinct_identity`), so
        // inspect the child's value instead of comparing the `Vec`.
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_name(), Some(b"FlateDecode".to_vec()));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_array_len_counts_in_place_and_keeps_none_for_a_non_array() {
        // The count qpdf reads off the borrowed array
        // (`getArrayNItems` → `asArray()->size()`,
        // `libqpdf/QPDFObjectHandle.cc:758-768`), including the empty array
        // `QPDF_Stream::filterable` special-cases at
        // `libqpdf/QPDF_Stream.cc:443`.
        let array = ObjectHandle::array(vec![
            ObjectHandle::name(b"FlateDecode".to_vec()),
            ObjectHandle::name(b"ASCII85Decode".to_vec()),
        ]);
        assert_eq!(array.try_array_len().unwrap(), Some(2));
        // Counting must not consume or replace the value.
        assert_eq!(array.as_array().map(|items| items.len()), Some(2));

        assert_eq!(
            ObjectHandle::array(Vec::new()).try_array_len().unwrap(),
            Some(0)
        );

        // Deliberately *not* qpdf's non-array answer. `getArrayNItems` warns
        // `typeWarning("array", "treating as empty")` and returns 0
        // (`libqpdf/QPDFObjectHandle.cc:763-766`); returning `Some(0)` here
        // would make `stream_filter::decode_filter_specs_from_handle` read a
        // scalar `/Filter` as an empty chain — an accepted unfiltered stream —
        // instead of raising its type error.
        for non_array in [
            ObjectHandle::null(),
            ObjectHandle::integer(1),
            ObjectHandle::name(b"FlateDecode".to_vec()),
            ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]),
        ] {
            assert_eq!(non_array.try_array_len().unwrap(), None);
        }
    }

    #[test]
    fn try_array_len_resolves_an_indirect_array_through_its_document() {
        let (handle, _resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![ObjectHandle::null()]));

        // Nothing non-resolving can see through an unresolved handle, so
        // dropping `try_dereference` from `try_array_len` reports this array
        // as "not an array" — the mutation the `stream_filter` call sites
        // cannot kill on their own, because a preceding `try_*` has already
        // resolved the slot by the time they count it.
        assert!(handle.as_array().is_none());
        assert!(!handle.is_resolved());

        assert_eq!(handle.try_array_len().unwrap(), Some(1));
        assert!(handle.is_resolved());
    }

    #[test]
    fn try_array_item_returns_live_first_middle_and_last_handles() {
        let first = ObjectHandle::integer(1);
        let middle = ObjectHandle::integer(2);
        let last = ObjectHandle::integer(3);
        let array = ObjectHandle::array(vec![first.clone(), middle.clone(), last.clone()]);

        assert!(array.try_array_item(0).unwrap().unwrap().ptr_eq(&first));
        assert!(array.try_array_item(1).unwrap().unwrap().ptr_eq(&middle));
        assert!(array.try_array_item(2).unwrap().unwrap().ptr_eq(&last));
    }

    #[test]
    fn try_array_item_returns_none_outside_the_valid_array_domain() {
        let array = ObjectHandle::array(vec![ObjectHandle::null()]);

        assert!(array.try_array_item(1).unwrap().is_none());
        assert!(ObjectHandle::integer(1)
            .try_array_item(0)
            .unwrap()
            .is_none());
    }

    #[test]
    fn try_array_item_resolves_an_indirect_holder_once_without_resolving_the_child() {
        let (child, _child_resolver) = error_resolving_handle(ObjectRef::new(23, 0));
        let (array, _resolver, calls) =
            logged_resolver_bearing_handle(ObjectValue::Array(vec![child.clone()]));

        let fetched = array.try_array_item(0).unwrap().unwrap();

        assert!(fetched.ptr_eq(&child));
        assert!(!fetched.is_resolved());
        assert_eq!(calls.borrow().as_slice(), &[ObjectRef::new(20, 0)]);
    }

    #[test]
    fn try_array_item_reports_a_dropped_document() {
        let (array, resolver) =
            resolver_bearing_handle(ObjectValue::Array(vec![ObjectHandle::null()]));
        drop(resolver);

        assert_eq!(
            array.try_array_item(0).unwrap_err().to_string(),
            "object 20 0 belongs to a dropped PDF"
        );
    }

    #[test]
    fn every_value_accessor_reports_a_dropped_document_rather_than_none() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(RecordingResolver::default());
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(21, 0),
            Rc::downgrade(&resolver),
        );
        drop(resolver);

        // A `None` here would be indistinguishable from a resolved value of
        // the wrong type, so each accessor must surface the dropped document.
        for error in [
            handle.try_as_name().unwrap_err(),
            handle.try_as_array().unwrap_err(),
            handle.try_array_len().unwrap_err(),
            handle.try_as_integer().unwrap_err(),
        ] {
            assert_eq!(error.to_string(), "object 21 0 belongs to a dropped PDF");
        }
        assert!(!handle.is_resolved());
    }

    #[test]
    fn direct_handle_clone_shares_identity_not_a_deep_copy() {
        let handle = ObjectHandle::integer(42);
        let clone = handle.clone();
        assert!(handle.ptr_eq(&clone));
    }

    #[test]
    fn two_direct_handles_with_equal_value_are_distinct_identity() {
        let a = ObjectHandle::integer(42);
        let b = ObjectHandle::integer(42);
        assert!(!a.ptr_eq(&b));
    }

    #[test]
    fn direct_handle_reports_direct_not_indirect() {
        let handle = ObjectHandle::integer(1);
        assert!(handle.is_direct());
        assert!(!handle.is_indirect());
        assert_eq!(handle.object_ref(), None);
    }

    #[test]
    fn indirect_handle_retains_object_ref_before_resolution() {
        let object_ref = ObjectRef::new(5, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, 0);
        assert!(handle.is_indirect());
        assert!(!handle.is_direct());
        assert_eq!(handle.object_ref(), Some(object_ref));
    }

    #[test]
    fn cloning_an_indirect_handle_shares_the_same_slot() {
        let object_ref = ObjectRef::new(5, 0);
        let handle = ObjectHandle::new_indirect_unresolved(object_ref, 0);
        let clone = handle.clone();
        assert!(handle.ptr_eq(&clone));
    }

    #[test]
    fn a_direct_and_an_indirect_handle_are_never_identical() {
        let direct = ObjectHandle::integer(42);
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), 0);
        assert!(!direct.ptr_eq(&indirect));
        assert!(!indirect.ptr_eq(&direct));
    }
}

#[cfg(test)]
mod uniform_identity_tests {
    use super::*;

    struct NoopResolver {
        calls: Rc<std::cell::Cell<usize>>,
    }

    impl DocumentResolver for NoopResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            _handle: &ObjectHandle,
        ) -> crate::Result<()> {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    fn resolver() -> Rc<dyn DocumentResolver> {
        Rc::new(NoopResolver {
            calls: Rc::new(std::cell::Cell::new(0)),
        })
    }

    fn recording_noop_resolver() -> (Rc<dyn DocumentResolver>, Rc<std::cell::Cell<usize>>) {
        let calls = Rc::new(std::cell::Cell::new(0));
        (
            Rc::new(NoopResolver {
                calls: calls.clone(),
            }),
            calls,
        )
    }

    struct ReenteringResolver {
        calls: Rc<RefCell<Vec<ObjectRef>>>,
    }

    impl DocumentResolver for ReenteringResolver {
        fn resolve_indirect(
            &self,
            object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            self.calls.borrow_mut().push(object_ref);
            assert_eq!(handle.object_ref(), Some(object_ref));
            handle.set_resolved(ObjectValue::Dictionary(Default::default()));
            handle.replace_key(b"/Resolved", ObjectHandle::boolean(true));
            Ok(())
        }
    }

    #[test]
    fn promotion_preserves_one_shared_object_identity_and_offset() {
        let resolver = resolver();
        let original =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(1))]);
        let outstanding_clone = original.clone();
        original.set_parsed_offset_if_unset(37);

        let promoted =
            original.promote_to_indirect(ObjectRef::new(17, 2), 41, Rc::downgrade(&resolver));

        assert!(original.is_same_object_as(&outstanding_clone));
        assert!(original.is_same_object_as(&promoted));
        assert!(original.is_indirect());
        assert!(outstanding_clone.is_indirect());
        assert_eq!(promoted.object_ref(), Some(ObjectRef::new(17, 2)));
        assert_eq!(outstanding_clone.get_parsed_offset(), 37);

        original.replace_key(b"/Value", ObjectHandle::integer(2));
        assert_eq!(promoted.get_key(b"/Value").as_integer(), Some(2));
        promoted.replace_key(b"/Value", ObjectHandle::integer(3));
        assert_eq!(outstanding_clone.get_key(b"/Value").as_integer(), Some(3));
    }

    #[test]
    fn promotion_does_not_clone_container_or_stream_storage() {
        let resolver = resolver();
        let array_child = ObjectHandle::dictionary(vec![]);
        let stream_dict = ObjectHandle::dictionary(vec![]);
        let stream_data = Rc::new(b"shared stream data".to_vec());
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: stream_dict.clone(),
            stream_data: Some(stream_data.clone()),
            stream_provider: None,
            stream_length: stream_data.len(),
        });
        let root = ObjectHandle::array(vec![array_child.clone(), stream.clone()]);

        let promoted =
            root.promote_to_indirect(ObjectRef::new(19, 0), 51, Rc::downgrade(&resolver));

        let children = promoted.as_array().expect("promoted array");
        assert!(children[0].is_same_object_as(&array_child));
        assert!(children[1].is_same_object_as(&stream));
        let promoted_dict = children[1].as_stream_dict().expect("stream dictionary");
        assert!(promoted_dict.is_same_object_as(&stream_dict));
        assert!(children[1].with_value(|value| matches!(value, Some(ObjectValue::Stream { stream_data: Some(actual), .. }) if Rc::ptr_eq(actual, &stream_data))));
    }

    #[test]
    fn promotion_delegates_unresolved_access_to_its_installed_resolver() {
        let (resolver, calls) = recording_noop_resolver();
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(20, 0), -1);
        handle.promote_to_indirect(ObjectRef::new(20, 0), 52, Rc::downgrade(&resolver));

        handle
            .try_dereference()
            .expect("the installed resolver accepts the lookup");

        assert_eq!(calls.get(), 1);
        assert!(
            !handle.is_resolved(),
            "a resolver that installs no value must not fabricate resolution"
        );
    }

    #[test]
    fn contained_children_observe_parent_promotion_without_edge_rewrite() {
        let resolver = resolver();
        let child = ObjectHandle::dictionary(vec![]);
        let parent = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);
        assert!(child.containing_object_refs_for_pdf(61).is_empty());

        parent.promote_to_indirect(ObjectRef::new(29, 0), 61, Rc::downgrade(&resolver));

        assert_eq!(
            child.containing_object_refs_for_pdf(61),
            vec![ObjectRef::new(29, 0)]
        );
    }

    #[test]
    fn re_promotion_updates_active_root_but_preserves_additive_provenance() {
        let first = resolver();
        let second = resolver();
        let child = ObjectHandle::dictionary(vec![]);
        let parent = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);

        let first_alias =
            parent.promote_to_indirect(ObjectRef::new(31, 0), 71, Rc::downgrade(&first));
        let second_alias =
            parent.promote_to_indirect(ObjectRef::new(37, 4), 72, Rc::downgrade(&second));

        assert!(first_alias.is_same_object_as(&second_alias));
        assert_eq!(parent.object_ref(), Some(ObjectRef::new(37, 4)));
        assert!(!parent.belongs_to_pdf(71));
        assert!(parent.belongs_to_pdf(72));
        assert!(child.belongs_to_pdf(71));
        assert!(child.belongs_to_pdf(72));
        assert!(child.containing_object_refs_for_pdf(71).is_empty());
        assert_eq!(
            child.containing_object_refs_for_pdf(72),
            vec![ObjectRef::new(37, 4)]
        );
    }

    #[test]
    fn promoted_child_is_an_indirect_boundary_not_a_direct_owner_path() {
        let resolver = resolver();
        let child = ObjectHandle::dictionary(vec![]);
        let outer = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);
        outer.promote_to_indirect(ObjectRef::new(41, 0), 81, Rc::downgrade(&resolver));
        child.promote_to_indirect(ObjectRef::new(43, 0), 81, Rc::downgrade(&resolver));

        assert!(child.containing_object_refs_for_pdf(81).is_empty());
        let grandchild = ObjectHandle::integer(1);
        child.replace_key(b"/Grandchild", grandchild.clone());
        assert_eq!(
            grandchild.containing_object_refs_for_pdf(81),
            vec![ObjectRef::new(43, 0)]
        );
    }

    #[test]
    fn dormant_parent_edge_tracks_removal_while_child_is_indirect() {
        let resolver = resolver();
        let child = ObjectHandle::dictionary(vec![]);
        let outer = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);
        outer.promote_to_indirect(ObjectRef::new(73, 0), 111, Rc::downgrade(&resolver));
        child.promote_to_indirect(ObjectRef::new(79, 0), 112, Rc::downgrade(&resolver));

        assert!(child.containing_object_refs_for_pdf(111).is_empty());
        child.disconnect();
        assert_eq!(
            child.containing_object_refs_for_pdf(111),
            vec![ObjectRef::new(73, 0)]
        );

        child.promote_to_indirect(ObjectRef::new(83, 0), 112, Rc::downgrade(&resolver));
        outer.remove_key(b"/Child");
        child.disconnect();
        assert!(child.containing_object_refs_for_pdf(111).is_empty());
    }

    #[test]
    fn disconnect_clears_indirect_metadata_for_every_non_null_alias() {
        let resolver = resolver();
        let original = ObjectHandle::integer(9);
        original.set_parsed_offset_if_unset(44);
        let promoted =
            original.promote_to_indirect(ObjectRef::new(47, 0), 91, Rc::downgrade(&resolver));

        promoted.disconnect();

        assert!(original.is_same_object_as(&promoted));
        assert!(original.is_direct());
        assert_eq!(original.object_ref(), None);
        assert!(!original.is_null());
        assert_eq!(original.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn disconnect_preserves_literal_null_and_missing_as_null() {
        let resolver = resolver();
        let literal_null = ObjectHandle::null();
        literal_null.set_parsed_offset_if_unset(55);
        literal_null.promote_to_indirect(ObjectRef::new(49, 0), 92, Rc::downgrade(&resolver));
        literal_null.disconnect();
        assert!(literal_null.is_direct());
        assert!(literal_null.is_null());
        assert_eq!(literal_null.get_parsed_offset(), 55);

        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(51, 0), -1);
        missing.set_missing();
        missing.promote_to_indirect(ObjectRef::new(53, 0), 93, Rc::downgrade(&resolver));
        missing.disconnect();
        assert!(missing.is_direct());
        assert!(missing.is_null());
        assert_eq!(missing.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn disconnect_of_an_unresolved_indirect_alias_becomes_destroyed_direct() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(54, 0), -1);
        let alias = handle.clone();
        handle.set_parsed_offset_if_unset(66);

        handle.disconnect();

        assert!(alias.is_same_object_as(&handle));
        assert!(alias.is_direct());
        assert_eq!(alias.object_ref(), None);
        assert!(!alias.is_null());
        assert_eq!(alias.type_code(), 14);
        assert_eq!(alias.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn disconnect_of_a_repromoted_destroyed_handle_resets_its_new_offset() {
        let resolver = resolver();
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(56, 0), -1);
        handle.disconnect();
        handle.promote_to_indirect(ObjectRef::new(58, 0), 94, Rc::downgrade(&resolver));
        handle.set_parsed_offset_if_unset(77);
        assert!(handle.is_indirect());
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(58, 0)));
        assert_eq!(handle.type_code(), 14);
        assert_eq!(handle.get_parsed_offset(), 77);

        handle.disconnect();

        assert!(handle.is_direct());
        assert_eq!(handle.object_ref(), None);
        assert!(!handle.is_null());
        assert_eq!(handle.type_code(), 14);
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn destroyed_direct_handle_has_no_legacy_value_to_clone_or_consume() {
        let resolver = resolver();
        let handle = ObjectHandle::integer(1);
        handle.promote_to_indirect(ObjectRef::new(55, 0), 94, Rc::downgrade(&resolver));
        handle.disconnect();

        assert!(handle.direct_value_clone().expect("destroyed").is_none());
        assert!(!handle.is_null(), "destroyed is distinct from literal null");
        assert!(handle.into_direct_value().is_none());
    }

    #[test]
    fn replace_direct_value_reinstalls_a_destroyed_direct_slot_payload() {
        let resolver = resolver();
        let handle = ObjectHandle::integer(1);
        handle.promote_to_indirect(ObjectRef::new(56, 0), 94, Rc::downgrade(&resolver));
        handle.disconnect();

        handle.replace_direct_value(ObjectValue::Integer(2));

        assert_eq!(handle.as_integer(), Some(2));
    }

    #[test]
    fn replacing_a_destroyed_direct_slot_attaches_nested_children_to_its_indirect_root() {
        let resolver = resolver();
        let root_ref = ObjectRef::new(57, 0);
        let root = ObjectHandle::new_indirect_for_pdf_with_resolver(
            root_ref,
            NO_PARSED_OFFSET,
            95,
            Rc::downgrade(&resolver),
        );
        let terminal = ObjectHandle::integer(1);
        root.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Terminal".to_vec(), terminal.clone()),
        ])));
        terminal.promote_to_indirect(ObjectRef::new(58, 0), 96, Rc::downgrade(&resolver));
        terminal.disconnect();

        let leaf = ObjectHandle::integer(2);
        terminal.replace_direct_value(ObjectValue::Dictionary(
            [(b"Nested".to_vec(), ObjectHandle::array(vec![leaf.clone()]))]
                .into_iter()
                .collect(),
        ));

        assert_eq!(leaf.containing_object_refs_for_pdf(95), vec![root_ref]);
    }

    #[test]
    fn promotion_records_identity_on_a_destroyed_direct_child_without_descending() {
        let resolver = resolver();
        let child = ObjectHandle::integer(1);
        child.promote_to_indirect(ObjectRef::new(57, 0), 95, Rc::downgrade(&resolver));
        child.disconnect();
        let parent = ObjectHandle::dictionary(vec![(b"Child".to_vec(), child.clone())]);

        parent.promote_to_indirect(ObjectRef::new(59, 0), 96, Rc::downgrade(&resolver));

        assert!(child.belongs_to_pdf(96));
        assert_eq!(
            child.containing_object_refs_for_pdf(96),
            vec![ObjectRef::new(59, 0)]
        );
    }

    #[test]
    fn resolution_state_is_shared_by_every_alias() {
        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(23, 0), -1);
        let alias = unresolved.clone();
        unresolved.set_resolved(ObjectValue::Integer(7));
        assert!(alias.is_same_object_as(&unresolved));
        assert!(alias.is_resolved());
        assert_eq!(alias.as_integer(), Some(7));
    }

    #[test]
    fn re_promotion_uses_latest_resolver() {
        let first_calls = Rc::new(RefCell::new(Vec::new()));
        let first: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
            calls: first_calls.clone(),
        });
        let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(59, 0),
            NO_PARSED_OFFSET,
            101,
            Rc::downgrade(&first),
        );
        let latest_calls = Rc::new(RefCell::new(Vec::new()));
        let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
            calls: latest_calls.clone(),
        });
        let alias = handle.promote_to_indirect(ObjectRef::new(61, 7), 102, Rc::downgrade(&latest));
        drop(first);

        alias.try_dereference().expect("latest resolver resolves");

        assert!(handle.is_same_object_as(&alias));
        assert_eq!(*first_calls.borrow(), Vec::<ObjectRef>::new());
        assert_eq!(*latest_calls.borrow(), vec![ObjectRef::new(61, 7)]);
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(61, 7)));
        assert_eq!(handle.get_key(b"/Resolved").as_boolean(), Some(true));
    }

    #[test]
    fn resolver_reentry_uses_latest_metadata_without_borrow_panic() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
            calls: calls.clone(),
        });
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(63, 0), -1);
        handle.promote_to_indirect(ObjectRef::new(67, 3), 103, Rc::downgrade(&latest));

        handle.try_dereference().expect("reentrant resolver");

        assert_eq!(*calls.borrow(), vec![ObjectRef::new(67, 3)]);
        assert_eq!(handle.get_key(b"/Resolved").as_boolean(), Some(true));
    }

    #[test]
    fn dropped_latest_resolver_reports_latest_object_and_stays_unresolved() {
        let first = resolver();
        let latest_calls = Rc::new(RefCell::new(Vec::new()));
        let latest: Rc<dyn DocumentResolver> = Rc::new(ReenteringResolver {
            calls: latest_calls.clone(),
        });
        let handle = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(69, 0),
            NO_PARSED_OFFSET,
            104,
            Rc::downgrade(&first),
        );
        handle.promote_to_indirect(ObjectRef::new(71, 5), 105, Rc::downgrade(&latest));
        drop(latest);

        let error = handle
            .try_dereference()
            .expect_err("latest owner was dropped");

        assert_eq!(error.to_string(), "object 71 5 belongs to a dropped PDF");
        assert!(latest_calls.borrow().is_empty());
        assert!(!handle.is_resolved());
    }
}

#[cfg(test)]
mod object_value_tests {
    use super::*;

    #[test]
    fn integer_handle_round_trips_its_value() {
        let handle = ObjectHandle::integer(42);
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn array_handle_holds_child_handles_not_raw_values() {
        let child = ObjectHandle::integer(7);
        let array = ObjectHandle::array(vec![child.clone()]);
        let children = array.as_array().expect("array");
        assert_eq!(children.len(), 1);
        assert!(children[0].ptr_eq(&child));
    }

    #[test]
    fn dictionary_handle_preserves_insertion_of_child_handles() {
        let value = ObjectHandle::name(b"Type".to_vec());
        let dict = ObjectHandle::dictionary(vec![(b"Key".to_vec(), value.clone())]);
        let entries = dict.as_dictionary().expect("dictionary");
        assert!(entries.get(b"/Key".as_slice()).unwrap().ptr_eq(&value));
    }

    #[test]
    fn dictionary_handle_normalizes_aliases_before_last_value_wins_collection() {
        let dict = ObjectHandle::dictionary(vec![
            (b"K".to_vec(), ObjectHandle::integer(1)),
            (b"/K".to_vec(), ObjectHandle::integer(2)),
        ]);

        assert_eq!(dict.get_key(b"/K").as_integer(), Some(2));
    }

    #[test]
    fn null_handle_is_null() {
        assert!(ObjectHandle::null().is_null());
        assert!(!ObjectHandle::integer(0).is_null());
    }

    #[test]
    fn stream_handle_round_trips_its_dict_and_data() {
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(3))]);
        let stream = ObjectHandle::stream(dict.clone(), Rc::new(b"abc".to_vec()));
        assert!(stream.as_stream_dict().expect("stream dict").ptr_eq(&dict));
        assert_eq!(stream.as_stream_data(), Some(Rc::new(b"abc".to_vec())));
        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("replaced stream data")
                .as_slice(),
            b"abc"
        );
        assert_eq!(stream.type_code(), 10, "ot_stream");
    }

    #[test]
    fn raw_stream_data_uses_replacements_including_an_empty_buffer() {
        let stream = ObjectHandle::stream(ObjectHandle::dictionary(vec![]), Rc::new(Vec::new()));
        assert!(
            stream
                .get_raw_stream_data()
                .expect("empty replacement data")
                .is_empty(),
            "an empty replacement is data, not an original-stream sentinel"
        );

        stream.replace_stream_data(Rc::new(b"replacement".to_vec()), None, None);
        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("replacement data")
                .as_slice(),
            b"replacement",
            "replacement data wins over every original-source detail"
        );
    }

    #[test]
    fn materialize_is_now_a_public_bridge_usable_outside_this_crate() {
        // `materialize` was widened from `pub(crate)` to `pub` so
        // `flpdf-qtest-tools`' qtest driver can bridge one `ObjectHandle`
        // value (a stream's own dictionary handle) back to a legacy
        // `Object`/`Dictionary` for its still-`&Dictionary`-shaped
        // filter/`DecodeParms` resolution routine — see this method's own
        // doc. `object_value_tests` above already exercises its per-variant
        // behavior exhaustively; this only pins the visibility contract.
        let handle = ObjectHandle::integer(1);
        let _: Object = ObjectHandle::materialize(&handle).expect("scalar materializes");
    }

    #[test]
    fn materialize_caps_a_direct_tree_deeper_than_any_parseable_document() {
        // Codex Review on PR #610 reproduced a process-aborting stack
        // overflow by building a 100,000-level-deep direct array through
        // the public `ObjectHandle::array` factory (which imposes no depth
        // bound the way parsed input does), calling the newly-public
        // `materialize` on it, then letting both the input handle and the
        // materialized result drop normally. `materialize` now caps its own
        // recursion at `parser::MAX_PARSE_DEPTH`, substituting `Object::Null`
        // past that point -- verified below to actually take effect, not
        // merely fail to crash by luck.
        //
        // `std::mem::forget(handle)` isolates what this fix is actually
        // responsible for: dropping the *input* `handle` here, built the
        // same way, independently overflows the stack even with no call to
        // `materialize` at all (confirmed while narrowing this down) --
        // `ObjectHandle`'s own recursive `Drop` is unprotected the same way
        // `Object`'s is, and was already reachable this way before this PR
        // (`array`/`dictionary` were already public). That is a real,
        // separate, pre-existing gap this fix does not and cannot close
        // from inside `materialize` -- forgetting `handle` here keeps this
        // test scoped to materialize's own contribution rather than
        // silently also depending on a fix for the unrelated one.
        let mut handle = ObjectHandle::integer(1);
        for _ in 0..100_000 {
            handle = ObjectHandle::array(vec![handle]);
        }

        let materialized = handle.materialize().expect("direct tree materializes");
        std::mem::forget(handle);

        let mut cursor = &materialized;
        let mut depth = 0;
        loop {
            match cursor {
                Object::Array(items) if items.len() == 1 => {
                    cursor = &items[0];
                    depth += 1;
                }
                other => {
                    assert_eq!(
                        *other,
                        Object::Null,
                        "the cap must substitute null once nesting exceeds MAX_PARSE_DEPTH"
                    );
                    break;
                }
            }
        }
        assert_eq!(
            depth,
            crate::parser::MAX_PARSE_DEPTH + 1,
            "materialize should recurse exactly through the depth cap before substituting null"
        );
    }

    #[test]
    fn real_literal_handle_preserves_the_non_canonical_source_literal() {
        // Object::RealLiteral exists so a non-canonical source spelling
        // (e.g. ".4") survives unparse byte-identically. The handle payload
        // must carry the same two fields, or byte-identical output breaks
        // the moment a real-literal round-trips through this layer.
        let handle = ObjectHandle::real_literal(0.4, b".4".to_vec());
        assert_eq!(handle.as_real_literal(), Some((0.4, b".4".to_vec())));
    }

    #[test]
    fn accessors_return_none_for_a_mismatched_direct_value() {
        // `as_integer`/`as_array`/`as_dictionary`/`as_real_literal`/
        // `as_stream_dict`/`as_stream_data` must reject a direct value of
        // the wrong variant, not just a missing one — the same `_ => None`
        // arm handles both cases.
        let handle = ObjectHandle::string(b"not-an-integer".to_vec());
        assert_eq!(handle.as_integer(), None);
        assert!(handle.as_array().is_none());
        assert!(handle.as_dictionary().is_none());
        assert_eq!(handle.as_real_literal(), None);
        assert!(handle.as_stream_dict().is_none());
        assert!(handle.as_stream_data().is_none());
    }

    #[test]
    fn accessors_return_none_for_an_indirect_handle_before_resolution() {
        // `with_value` never performs hidden I/O to resolve an indirect
        // handle (design, `Pdf` section), so today every indirect handle
        // reads as "value not known" — surfaced as `None` from the typed
        // accessors. `is_null` is not an exception: an unresolved indirect
        // handle is not assumed to be null (matches qpdf's `isDirectNull`).
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert!(!handle.is_null());
        assert_eq!(handle.as_integer(), None);
        assert!(handle.as_array().is_none());
        assert!(handle.as_dictionary().is_none());
        assert_eq!(handle.as_real_literal(), None);
        assert!(handle.as_stream_dict().is_none());
        assert!(handle.as_stream_data().is_none());
    }
}

#[cfg(test)]
mod stream_payload_sharing_tests {
    use super::*;

    // Every assertion below compares buffer identity against the buffer the
    // test itself created, never bytes: a byte-equality assertion passes for a
    // deep-copying implementation too.
    fn payload_of(handle: &ObjectHandle) -> Rc<Vec<u8>> {
        handle.as_stream_data().expect("stream value")
    }

    fn length_dict(len: usize) -> ObjectHandle {
        ObjectHandle::dictionary(vec![(
            b"Length".to_vec(),
            ObjectHandle::integer(len as i64),
        )])
    }

    // `QPDF::copyStreamData` takes the source stream's buffer
    // (`libqpdf/QPDF.cc:2240`) and installs it on a second stream — in a
    // different document — with no byte copy (`:2256-2258`), because "if the
    // source stream is copied multiple times, we don't have to keep
    // duplicating the memory" (`:2242-2244`). That is only expressible when
    // both the accessor and the replacement entry point speak in shared
    // buffers, as qpdf's own `getStreamDataBuffer` /
    // `replaceStreamData(std::shared_ptr<Buffer>, ...)` pair does.
    #[test]
    fn one_buffer_backs_two_streams_without_copying() {
        let shared = Rc::new(vec![0x5a; 4096]);
        let source = ObjectHandle::stream(length_dict(4096), Rc::clone(&shared));
        let destination = ObjectHandle::stream(length_dict(0), Rc::new(Vec::new()));

        destination.replace_stream_data(source.as_stream_data().expect("stream data"), None, None);

        assert!(Rc::ptr_eq(&payload_of(&source), &shared));
        assert!(Rc::ptr_eq(&payload_of(&destination), &shared));
        assert_eq!(
            destination
                .as_stream_dict()
                .expect("stream dict")
                .get_key(b"/Length")
                .as_integer(),
            Some(4096),
        );
    }

    // qpdf's payload is a `std::shared_ptr<Buffer>`
    // (`libqpdf/qpdf/QPDF_Stream.hh:104`), so copying a stream value shares
    // the bytes instead of duplicating them. The dictionary cannot be shared
    // the same way: it is an `ObjectHandle`, so a shared one would carry a
    // later `replace_stream_data`'s `/Length` across to a slot whose payload
    // field did not change. Sharing the payload is safe precisely because
    // replacement swaps the field rather than mutating the buffer.
    #[test]
    fn direct_value_clone_shares_the_stream_payload_but_not_the_dictionary() {
        let shared = Rc::new(vec![0x5a; 4096]);
        let stream = ObjectHandle::stream(length_dict(4096), Rc::clone(&shared));
        let source_dict = stream.as_stream_dict().expect("source stream dict");

        let copy = ObjectHandle::from_value(
            stream
                .direct_value_clone()
                .expect("stream dict privatizes")
                .expect("direct value"),
        );

        assert!(Rc::ptr_eq(&payload_of(&copy), &shared));
        let copy_dict = copy.as_stream_dict().expect("copied stream dict");
        assert!(!copy_dict.is_same_object_as(&source_dict));
        copy_dict.replace_key(b"/Length", ObjectHandle::integer(7));
        assert_eq!(
            source_dict.get_key(b"/Length").as_integer(),
            Some(4096),
            "each slot's dictionary describes only its own payload"
        );
    }

    // The privatizing copy is `shallowCopy` on a dictionary, so a *direct*
    // stream inside the stream dictionary hits `QPDF_Dictionary::copy`'s own
    // `shallowCopy` of each direct child and the same `QPDF_Stream::copy`
    // throw (`libqpdf/QPDF_Stream.cc:140-145`).
    #[test]
    fn direct_value_clone_propagates_a_nested_direct_streams_rejection() {
        let inner = ObjectHandle::stream(length_dict(3), Rc::new(b"abc".to_vec()));
        let outer = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"Nested".to_vec(), inner)]),
            Rc::new(b"xyz".to_vec()),
        );

        let error = outer
            .direct_value_clone()
            .expect_err("a direct stream in the stream dictionary is refused");

        assert!(
            matches!(error, Error::System(ref message)
                if message == "stream objects cannot be cloned"),
            "{error:?}"
        );
    }

    // `QPDF_Stream::copy` (`libqpdf/QPDF_Stream.cc:140-145`) ignores its
    // `shallow` argument and unconditionally throws
    // `std::runtime_error("stream objects cannot be cloned")`, so
    // `shallowCopy` has no stream path to reach.
    #[test]
    fn shallow_copy_refuses_a_stream() {
        let shared = Rc::new(vec![0x5a; 4096]);
        let stream = ObjectHandle::stream(length_dict(4096), Rc::clone(&shared));

        let error = stream.shallow_copy().expect_err("streams cannot be cloned");

        assert!(
            matches!(error, Error::System(ref message)
                if message == "stream objects cannot be cloned"),
            "qpdf throws std::runtime_error here, which this crate classifies \
             as Error::System: {error:?}"
        );
    }

    // A stream reached through the recursion is refused for the same reason:
    // `QPDF_Dictionary::copy`/`QPDF_Array::copy` shallow-copy each *direct*
    // child, so the throw comes from the same `QPDF_Stream::copy`.
    #[test]
    fn shallow_copy_refuses_a_direct_stream_nested_in_a_container() {
        let stream = ObjectHandle::stream(length_dict(4096), Rc::new(vec![0x5a; 4096]));
        let dictionary = ObjectHandle::dictionary(vec![(b"Nested".to_vec(), stream.clone())]);
        let array = ObjectHandle::array(vec![ObjectHandle::array(vec![stream])]);

        for container in [dictionary, array] {
            let error = container
                .shallow_copy()
                .expect_err("a direct stream descendant is refused too");
            assert!(
                matches!(error, Error::System(ref message)
                    if message == "stream objects cannot be cloned"),
                "{error:?}"
            );
        }
    }

    // An *indirect* stream child is not copied at all — it keeps its shared
    // identity, exactly as `QPDF_Dictionary::copy`'s `value.isIndirect()`
    // arm does — so the container copy succeeds.
    #[test]
    fn shallow_copy_keeps_an_indirect_stream_child_shared() {
        let stream = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        stream.set_resolved(ObjectValue::Stream {
            stream_dict: length_dict(4096),
            stream_data: Some(Rc::new(vec![0x5a; 4096])),
            stream_provider: None,
            stream_length: 4096,
        });
        let dictionary = ObjectHandle::dictionary(vec![(b"Nested".to_vec(), stream.clone())]);

        let copy = dictionary
            .shallow_copy()
            .expect("an indirect stream child is shared, never copied");

        assert!(copy.get_key(b"/Nested").is_same_object_as(&stream));
    }

    // `QPDF_Stream::getStreamDataBuffer` (`libqpdf/qpdf/QPDF_Stream.hh:39`)
    // hands out the `shared_ptr` itself, which is what lets
    // `QPDF::copyStreamData` (`libqpdf/QPDF.cc:2240,2256-2258`) give one
    // buffer to a second stream without duplicating the memory.
    #[test]
    fn as_stream_data_hands_out_the_stored_payload_without_copying_it() {
        let shared = Rc::new(vec![0x5a; 4096]);
        let stream = ObjectHandle::stream(length_dict(4096), Rc::clone(&shared));

        let handed_out = stream.as_stream_data().expect("stream data");

        assert!(Rc::ptr_eq(&handed_out, &shared));
    }

    // An empty payload is still a buffer, not an absent one: it is handed out
    // and shared like any other. qpdf nevertheless treats its zero byte length
    // as the unknown-length boundary and removes `/Length`.
    #[test]
    fn an_empty_payload_is_shared_like_any_other() {
        let shared = Rc::new(Vec::new());
        let stream = ObjectHandle::stream(length_dict(4096), Rc::new(vec![0x5a; 4096]));

        stream.replace_stream_data(Rc::clone(&shared), None, None);

        assert!(Rc::ptr_eq(&payload_of(&stream), &shared));
        assert!(!stream
            .as_stream_dict()
            .expect("stream dict")
            .has_key(b"/Length"));
    }
}

#[cfg(test)]
mod parsed_offset_tests {
    use super::*;

    #[test]
    fn public_factory_direct_handles_default_to_no_offset_sentinel() {
        for handle in [
            ObjectHandle::null(),
            ObjectHandle::boolean(true),
            ObjectHandle::integer(1),
            ObjectHandle::real(1.5),
            ObjectHandle::name(b"Foo".to_vec()),
            ObjectHandle::string(b"bar".to_vec()),
            ObjectHandle::array(Vec::new()),
            ObjectHandle::dictionary(Vec::new()),
        ] {
            assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        }
    }

    #[test]
    fn new_indirect_unresolved_starts_at_no_offset_sentinel() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn set_parsed_offset_is_retained_once_set() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn first_nonnegative_offset_is_retained_a_second_set_is_ignored() {
        // "The first nonnegative offset assigned to a value is retained.
        // Resolution, cache access, unparse, and writer planning do not
        // recompute or replace it." (design, Parsed-Offset Contract)
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn indirect_handle_honors_the_same_set_once_contract() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_parsed_offset_if_unset(100);
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn zero_is_a_legitimate_parsed_offset_not_treated_as_unset() {
        // The guard is a strict `< 0` check, so `0` (a real token-start
        // offset) must count as "already set" and block later writes, the
        // same as any other non-negative value.
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(0);
        assert_eq!(handle.get_parsed_offset(), 0);
        handle.set_parsed_offset_if_unset(50);
        assert_eq!(handle.get_parsed_offset(), 0);
    }
}

#[cfg(test)]
mod resolution_state_tests {
    use super::*;

    #[test]
    fn direct_handle_is_always_resolved() {
        // A direct handle has no resolution state to wait on — its value was
        // known at construction time.
        assert!(ObjectHandle::integer(1).is_resolved());
    }

    #[test]
    fn into_direct_value_leaves_child_edges_attached_when_the_parent_is_shared() {
        let child = ObjectHandle::integer(1);
        let parent = ObjectHandle::array(vec![child.clone()]);
        let retained_parent = parent.clone();

        assert!(parent.into_direct_value().is_none());

        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, NO_PARSED_OFFSET);
        owner.set_resolved(ObjectValue::Array(vec![retained_parent]));
        assert_eq!(child.containing_object_refs(), vec![owner_ref]);
    }

    #[test]
    fn fresh_indirect_handle_is_not_resolved() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert!(!handle.is_resolved());
    }

    #[test]
    fn set_resolved_marks_the_handle_resolved_and_exposes_its_value() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        assert!(handle.is_resolved());
        assert_eq!(handle.as_integer(), Some(7));
    }

    #[test]
    fn set_missing_marks_the_handle_resolved_to_null() {
        // `Missing` (dangling/broken reference) must present the same
        // observable value as a genuinely parsed `null` object — but see
        // `set_resolved_with_a_null_value_is_indistinguishable_from_the_outside`
        // for proof the two routes are not literally the same variant.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_missing();
        assert!(handle.is_resolved());
        assert!(handle.is_null());
        assert_eq!(handle.as_integer(), None);
    }

    /// The two null routes are distinct [`ObjectState`] variants that no
    /// public observation can tell apart.
    ///
    /// Named by `set_missing_marks_the_handle_resolved_to_null`'s comment,
    /// which cited it before it existed. Both halves matter and neither
    /// implies the other: the *indistinguishable* half is what lets
    /// `reader/resolver.rs` pick `set_missing` for qpdf's loop branch, where
    /// qpdf caches a live `QPDF_Null` (`libqpdf/QPDF.cc:1711`); the *distinct
    /// variant* half is what makes `with_value_mut` behave differently between
    /// them, which nothing can observe today only because every caller matches
    /// on a container variant `Null` is not.
    #[test]
    fn set_resolved_with_a_null_value_is_indistinguishable_from_the_outside() {
        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        missing.set_missing();
        let null = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        null.set_resolved(ObjectValue::Null);

        for observation in [
            ObjectHandle::is_resolved,
            ObjectHandle::is_null,
            ObjectHandle::is_indirect,
            ObjectHandle::is_direct,
        ] {
            assert_eq!(observation(&missing), observation(&null));
        }
        assert_eq!(missing.as_integer(), null.as_integer());
        assert_eq!(missing.as_array().is_none(), null.as_array().is_none());
        assert_eq!(
            missing.as_dictionary().is_none(),
            null.as_dictionary().is_none()
        );
        assert_eq!(missing.as_stream_data(), null.as_stream_data());
        assert_eq!(missing.type_code(), null.type_code());
        assert_eq!(missing.unparse_resolved(), null.unparse_resolved());
        assert_eq!(missing.get_parsed_offset(), null.get_parsed_offset());

        // Asserted with `matches!` rather than by mapping the state to a name:
        // a mapping needs arms for the two variants neither handle can be in,
        // and an arm nothing reaches is an uncovered line.
        assert!(
            matches!(&*missing.0.borrow().state.borrow(), ObjectState::Missing),
            "`set_missing` must leave the slot in the `Missing` variant"
        );
        let state = null.0.borrow().state.clone();
        assert!(
            matches!(&*state.borrow(), ObjectState::Resolved(ObjectValue::Null)),
            "`set_resolved(Null)` must leave the slot in the `Resolved` variant"
        );
    }

    #[test]
    fn set_missing_resets_a_previously_recorded_parsed_offset() {
        // Design's Parsed-Offset Contract: "An absent, freed, dangling,
        // cyclic, or otherwise unresolvable indirect object ... resolves to
        // null with parsed offset -1." A handle that was already resolved
        // with a real (non-negative) offset -- e.g. natively parsed, then
        // later deleted -- must not keep reporting its former body's source
        // position once it reads as null.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);

        handle.set_missing();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert!(handle.is_null());
    }

    #[test]
    fn set_resolved_and_set_missing_are_a_no_op_on_a_direct_handle() {
        // Direct handles have no resolution state; calling either setter must
        // not panic and must leave the original value untouched.
        let handle = ObjectHandle::integer(42);
        handle.set_resolved(ObjectValue::Integer(99));
        handle.set_missing();
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn from_value_constructs_a_direct_handle_at_the_offset_sentinel() {
        let handle = ObjectHandle::from_value(ObjectValue::Integer(3));
        assert!(handle.is_direct());
        assert_eq!(handle.as_integer(), Some(3));
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn disconnect_replaces_a_resolved_value_with_destroyed_state() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));

        handle.disconnect();

        assert!(handle.is_resolved());
        assert!(!handle.is_null());
        assert_eq!(handle.type_code(), 14);
        assert_eq!(handle.as_integer(), None);
    }

    #[test]
    fn disconnect_resets_a_previously_recorded_parsed_offset() {
        // Mirrors `set_missing_resets_a_previously_recorded_parsed_offset`
        // for the same Parsed-Offset Contract clause: a handle a caller
        // keeps alive past its owning `Pdf`'s drop must not keep reporting
        // its former body's source position once it reads as null.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.set_parsed_offset_if_unset(100);
        assert_eq!(handle.get_parsed_offset(), 100);

        handle.disconnect();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn disconnect_clears_a_previously_recorded_description() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.set_description("input.pdf, object 1 0 at offset $PO".to_owned(), 100);
        assert!(handle.description().contains("offset"));

        handle.disconnect();

        assert_eq!(handle.description(), "");
    }

    #[test]
    fn disconnect_is_a_no_op_on_a_direct_handle() {
        let handle = ObjectHandle::integer(42);
        handle.disconnect();
        assert_eq!(handle.as_integer(), Some(42));
    }

    #[test]
    fn strong_count_reports_a_direct_handles_rc_count_too() {
        let handle = ObjectHandle::integer(1);
        assert_eq!(handle.strong_count(), 1);
        let clone = handle.clone();
        assert_eq!(handle.strong_count(), 2);
        drop(clone);
        assert_eq!(handle.strong_count(), 1);
    }

    #[test]
    fn disconnect_drops_the_strong_rc_a_resolved_value_holds_to_a_cyclic_child() {
        // Two objects that reference each other (e.g. a /Pages node and a
        // page's /Parent) form a strong Rc cycle once both are resolved:
        // each slot's value embeds the other's canonical handle. `disconnect`
        // (called by `Pdf::drop` for every registry entry) must sever that
        // cycle so both slots free once external references are gone.
        let a = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        let b = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        a.set_resolved(ObjectValue::Dictionary(
            [(b"Kid".to_vec(), b.clone())].into_iter().collect(),
        ));
        b.set_resolved(ObjectValue::Dictionary(
            [(b"Parent".to_vec(), a.clone())].into_iter().collect(),
        ));
        assert_eq!(a.strong_count(), 2, "held by this test and by b's value");
        assert_eq!(b.strong_count(), 2, "held by this test and by a's value");

        a.disconnect();
        b.disconnect();

        assert_eq!(a.strong_count(), 1, "only this test's own handle remains");
        assert_eq!(b.strong_count(), 1, "only this test's own handle remains");
    }

    #[test]
    fn debug_format_does_not_recurse_through_a_self_referential_handle() {
        // A one-object `/Self 1 0 R` dictionary: the handle's own resolved
        // value embeds itself. A derived `Debug` would recurse into the
        // same slot forever and overflow the stack; formatting must stop at
        // the indirect boundary instead.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Dictionary(
            [(b"Self".to_vec(), handle.clone())].into_iter().collect(),
        ));

        let formatted = format!("{handle:?}");

        assert!(formatted.contains("ObjectHandle::Indirect"));
        assert!(formatted.contains("Resolved(..)"));
    }

    #[test]
    fn debug_format_does_not_recurse_through_a_reciprocal_cycle() {
        let a = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        let b = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        a.set_resolved(ObjectValue::Dictionary(
            [(b"Kid".to_vec(), b.clone())].into_iter().collect(),
        ));
        b.set_resolved(ObjectValue::Dictionary(
            [(b"Parent".to_vec(), a.clone())].into_iter().collect(),
        ));

        let formatted = format!("{a:?}");

        assert!(formatted.contains("ObjectHandle::Indirect"));
    }

    #[test]
    fn debug_format_of_a_direct_handle_shows_its_value() {
        let handle = ObjectHandle::integer(7);
        assert!(format!("{handle:?}").contains("ObjectHandle::Direct"));
    }

    #[test]
    fn debug_format_summarizes_every_indirect_resolution_state() {
        let not_yet_resolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert!(format!("{not_yet_resolved:?}").contains("NotYetResolved"));

        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        missing.set_missing();
        assert!(format!("{missing:?}").contains("Missing"));

        let destroyed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(3, 0), 0);
        destroyed.set_resolved(ObjectValue::Integer(1));
        destroyed.disconnect();
        assert!(format!("{destroyed:?}").contains("Destroyed"));
    }
}

#[cfg(test)]
mod materialize_tests {
    use super::*;

    #[test]
    fn scalar_values_materialize_to_the_matching_object_variant() {
        assert_eq!(
            ObjectHandle::null()
                .materialize()
                .expect("null materializes"),
            Object::Null
        );
        assert_eq!(
            ObjectHandle::boolean(true)
                .materialize()
                .expect("boolean materializes"),
            Object::Boolean(true)
        );
        assert_eq!(
            ObjectHandle::integer(7)
                .materialize()
                .expect("integer materializes"),
            Object::Integer(7)
        );
        assert_eq!(
            ObjectHandle::real(1.5)
                .materialize()
                .expect("real materializes"),
            Object::Real(1.5)
        );
        assert_eq!(
            ObjectHandle::name(b"Foo".to_vec())
                .materialize()
                .expect("name materializes"),
            Object::Name(b"Foo".to_vec())
        );
        assert_eq!(
            ObjectHandle::string(b"bar".to_vec())
                .materialize()
                .expect("string materializes"),
            Object::String(b"bar".to_vec())
        );
    }

    #[test]
    fn real_literal_materializes_with_its_source_literal_preserved() {
        let handle = ObjectHandle::real_literal(0.4, b".4".to_vec());
        assert_eq!(
            handle.materialize().expect("real literal materializes"),
            Object::RealLiteral {
                value: 0.4,
                literal: b".4".to_vec(),
            }
        );
    }

    #[test]
    fn a_direct_array_materializes_recursively_but_an_indirect_child_becomes_a_reference() {
        let indirect_child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), indirect_child]);

        let materialized = array.materialize().expect("array materializes");
        assert_eq!(
            materialized,
            Object::Array(vec![
                Object::Integer(1),
                Object::Reference(ObjectRef::new(9, 0))
            ])
        );
    }

    #[test]
    fn a_dictionary_materializes_its_entries_by_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let Object::Dictionary(materialized) = dict.materialize().expect("dictionary materializes")
        else {
            panic!("expected a dictionary"); // cov:ignore: unreachable in a passing run
        };
        assert_eq!(materialized.get("A"), Some(&Object::Integer(1)));
    }

    #[test]
    fn a_stream_value_flattens_its_dictionary_handle_into_a_plain_dictionary() {
        let dict_handle =
            ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(5))]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict_handle,
            stream_data: Some(Rc::new(b"Hello".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });

        let Object::Stream(materialized) = stream.materialize().expect("stream materializes")
        else {
            panic!("expected a stream"); // cov:ignore: unreachable in a passing run
        };
        assert_eq!(materialized.data, b"Hello");
        assert_eq!(materialized.dict.get("Length"), Some(&Object::Integer(5)));
    }

    #[test]
    fn stream_materialization_degrades_a_non_dictionary_stream_dictionary() {
        let stream = ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(b"data".to_vec()));

        let Object::Stream(materialized) = stream.materialize().expect("stream materializes")
        else {
            panic!("expected a stream"); // cov:ignore: established by construction above
        };
        assert!(materialized.dict.iter().next().is_none());
        assert_eq!(materialized.data, b"data");
    }

    #[test]
    fn materialize_value_rejects_a_stream_without_its_source_handle() {
        let error = materialize_value(
            &ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![]),
                stream_data: Some(Rc::new(Vec::new())),
                stream_provider: None,
                stream_length: 0,
            },
            0,
        )
        .expect_err("the helper has no stream handle to read an original source from");
        assert!(matches!(error, Error::Internal(message)
            if message == "stream materialization must retain its ObjectHandle source"));
    }

    #[test]
    fn an_unresolved_indirect_handle_materializes_to_null_without_performing_resolution() {
        // `Pdf::resolve_borrowed` always resolves before materializing, but
        // `materialize` itself must not assume that precondition holds --
        // a caller that skips resolution sees `Object::Null`, not a panic.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert_eq!(
            handle
                .materialize()
                .expect("unresolved handle materializes"),
            Object::Null
        );
    }

    #[test]
    fn replace_direct_value_updates_a_direct_handles_value_but_keeps_its_offset() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);

        handle.replace_direct_value(ObjectValue::Integer(2));

        assert_eq!(handle.as_integer(), Some(2));
        assert_eq!(handle.get_parsed_offset(), 100);
    }

    #[test]
    fn replace_direct_value_moves_dictionary_storage_without_cloning_it() {
        let handle = ObjectHandle::integer(1);
        let mut key = vec![b'K'; 4_096];
        key[0] = b'/';
        let original_key_allocation = key.as_ptr();
        let value =
            ObjectValue::Dictionary([(key, ObjectHandle::integer(2))].into_iter().collect());

        handle.replace_direct_value(value);

        assert!(handle.is_direct());
        let slot = handle.0.borrow();
        let state = slot.state.clone();
        drop(slot);
        let state = state.borrow();
        let ObjectState::Resolved(ObjectValue::Dictionary(entries)) = &*state else {
            panic!("test handle must contain the supplied dictionary"); // cov:ignore: replacement fixes this value
        };
        let replaced_key_allocation = entries
            .keys()
            .next()
            .expect("replacement dictionary retains its key")
            .as_ptr();
        assert_eq!(replaced_key_allocation, original_key_allocation);
    }

    #[test]
    fn replace_direct_value_is_a_no_op_on_an_indirect_handle() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(1));

        handle.replace_direct_value(ObjectValue::Integer(2));

        assert_eq!(handle.as_integer(), Some(1));
    }

    #[test]
    fn reset_parsed_offset_clears_an_already_set_offset() {
        let handle = ObjectHandle::integer(1);
        handle.set_parsed_offset_if_unset(100);

        handle.reset_parsed_offset();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        // The set-once guard is not permanently defeated: a later value can
        // set a fresh offset after a reset.
        handle.set_parsed_offset_if_unset(200);
        assert_eq!(handle.get_parsed_offset(), 200);
    }

    #[test]
    fn reset_parsed_offset_works_on_an_indirect_handle_too() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_parsed_offset_if_unset(100);

        handle.reset_parsed_offset();

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }
}

#[cfg(test)]
mod rounded_accessor_tests {
    use super::*;

    #[test]
    fn boolean_handle_round_trips_its_value() {
        assert_eq!(ObjectHandle::boolean(true).as_boolean(), Some(true));
        assert_eq!(ObjectHandle::boolean(false).as_boolean(), Some(false));
        assert_eq!(ObjectHandle::integer(1).as_boolean(), None);
    }

    #[test]
    fn as_real_accepts_both_real_and_real_literal_like_object_does() {
        // Mirrors Object::as_real's own `Real(v) | RealLiteral { value: v, .. }`
        // arm (object.rs:348-353) — a real-literal value is still "a real"
        // for callers that don't care about the source spelling.
        assert_eq!(ObjectHandle::real(1.5).as_real(), Some(1.5));
        assert_eq!(
            ObjectHandle::real_literal(0.4, b".4".to_vec()).as_real(),
            Some(0.4)
        );
        assert_eq!(ObjectHandle::integer(1).as_real(), None);
    }

    #[test]
    fn name_and_string_handles_round_trip_their_bytes() {
        assert_eq!(
            ObjectHandle::name(b"Type".to_vec()).as_name(),
            Some(b"Type".to_vec())
        );
        assert_eq!(
            ObjectHandle::string(b"hi".to_vec()).as_string(),
            Some(b"hi".to_vec())
        );
        assert!(ObjectHandle::name(b"Type".to_vec()).as_string().is_none());
        assert!(ObjectHandle::string(b"hi".to_vec()).as_name().is_none());
    }

    #[test]
    fn as_reference_reads_a_resolved_indirect_redirect_but_not_a_plain_value() {
        // ObjectValue::Reference is what an indirect handle resolves to when
        // its own body is itself a bare reference (Pdf::set_object-driven
        // redirect/collapse chains — see ObjectValue::Reference's own doc).
        let redirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        redirect.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));
        assert_eq!(redirect.as_reference(), Some(ObjectRef::new(9, 0)));
        assert_eq!(ObjectHandle::integer(1).as_reference(), None);
    }

    #[test]
    fn rounded_accessors_return_none_for_an_indirect_handle_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert_eq!(handle.as_boolean(), None);
        assert_eq!(handle.as_real(), None);
        assert!(handle.as_name().is_none());
        assert!(handle.as_string().is_none());
        assert_eq!(handle.as_reference(), None);
    }
}

#[cfg(test)]
mod token_value_tests {
    use super::*;

    #[test]
    fn operator_handle_round_trips_its_bytes() {
        let handle = ObjectHandle::operator(b"q".to_vec());
        assert_eq!(handle.as_operator(), Some(b"q".to_vec()));
        assert!(handle.as_inline_image().is_none());
    }

    #[test]
    fn inline_image_handle_round_trips_its_bytes() {
        let handle = ObjectHandle::inline_image(b"\x00\x01raw".to_vec());
        assert_eq!(handle.as_inline_image(), Some(b"\x00\x01raw".to_vec()));
        assert!(handle.as_operator().is_none());
    }

    #[test]
    fn operator_and_inline_image_materialize_to_the_matching_object_variant() {
        assert_eq!(
            ObjectHandle::operator(b"Do".to_vec())
                .materialize()
                .expect("operator materializes"),
            Object::Operator(b"Do".to_vec())
        );
        assert_eq!(
            ObjectHandle::inline_image(b"data".to_vec())
                .materialize()
                .expect("inline image materializes"),
            Object::InlineImage(b"data".to_vec())
        );
    }
}

#[cfg(test)]
mod is_resolved_visibility_tests {
    use super::*;

    #[test]
    fn is_resolved_is_usable_the_same_way_a_pub_fn_is() {
        // This test doesn't exercise new behavior (resolution_state_tests
        // already covers is_resolved's semantics exhaustively) — it exists
        // only to keep a compile-time witness that `is_resolved` stays
        // `pub`, the same way the rest of this module's public surface has
        // a direct caller in-tree. Real external verification happens in
        // Task 7 (zero-consumer-diff gate does not apply to this file
        // itself, so a positive compile check here is the useful signal).
        let handle = ObjectHandle::integer(1);
        let _: bool = ObjectHandle::is_resolved(&handle);
    }
}

#[cfg(test)]
mod type_code_tests {
    use super::*;

    #[test]
    fn direct_scalar_and_container_type_codes_match_qpdf_ordinals() {
        // Ordinals and strings verified directly against the pinned qpdf
        // 11.9.0 source: `include/qpdf/Constants.h:108-127`
        // (`qpdf_object_type_e`) for the numbers, and each type's own
        // `libqpdf/QPDF_*.cc` `QPDFValue(::ot_*, "...")` constructor for the
        // name string (e.g. `libqpdf/QPDF_InlineImage.cc:6` for the
        // hyphenated `"inline-image"`).
        let cases: &[(ObjectHandle, u8, &str)] = &[
            (ObjectHandle::null(), 2, "null"),
            (ObjectHandle::boolean(true), 3, "boolean"),
            (ObjectHandle::integer(1), 4, "integer"),
            (ObjectHandle::real(1.5), 5, "real"),
            (ObjectHandle::real_literal(0.4, b".4".to_vec()), 5, "real"),
            (ObjectHandle::string(b"s".to_vec()), 6, "string"),
            (ObjectHandle::name(b"N".to_vec()), 7, "name"),
            (ObjectHandle::array(vec![]), 8, "array"),
            (ObjectHandle::dictionary(vec![]), 9, "dictionary"),
            (ObjectHandle::operator(b"q".to_vec()), 11, "operator"),
            (
                ObjectHandle::inline_image(b"d".to_vec()),
                12,
                "inline-image",
            ),
        ];
        for (handle, code, name) in cases {
            assert_eq!(handle.type_code(), *code, "{name}");
            assert_eq!(handle.type_name(), *name);
            assert!(!handle.is_reserved(), "ordinary {name} is not reserved");
        }
    }

    #[test]
    fn stream_handle_type_code_is_stream() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            stream_length: 0,
        });
        assert_eq!(stream.type_code(), 10);
        assert_eq!(stream.type_name(), "stream");
    }

    #[test]
    fn not_yet_resolved_indirect_handle_reports_unresolved_without_resolving() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert_eq!(handle.type_code(), 13, "ot_unresolved");
        assert_eq!(handle.type_name(), "unresolved");
        assert!(!handle.is_reserved());
    }

    #[test]
    fn destroyed_handle_reports_destroyed_after_indirect_metadata_is_cleared() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(1));
        handle.disconnect();
        assert_eq!(handle.type_code(), 14, "ot_destroyed");
        assert_eq!(handle.type_name(), "destroyed");
        assert!(!handle.is_reserved());
    }

    #[test]
    fn missing_indirect_handle_reports_null_not_a_distinct_missing_code() {
        // qpdf has no separate "missing" ot_* code — a dangling/broken
        // reference presents as ot_null, matching set_missing's own
        // documented is_null()==true contract.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_missing();
        assert_eq!(handle.type_code(), 2, "ot_null");
        assert_eq!(handle.type_name(), "null");
        assert!(!handle.is_reserved());
    }

    #[test]
    fn resolved_indirect_handle_reports_its_real_value_type() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        assert_eq!(handle.type_code(), 4, "ot_integer");
        assert_eq!(handle.type_name(), "integer");
    }

    #[test]
    fn resolved_to_a_reference_indirect_handle_reports_unresolved() {
        // `ObjectValue::Reference` is a real, reachable resolution state,
        // not a speculative one: `Pdf::set_object` (`reader.rs:1184-1239`)
        // is public, `Object::Reference` is a public variant, and
        // `set_object` writes exactly this state via
        // `handle.set_resolved(value)` (`reader.rs:1207-1210`) whenever the
        // lifted value is itself a bare reference
        // (`reader.rs:1877`'s `Object::Reference(object_ref) =>
        // ObjectValue::Reference(*object_ref)` arm) — e.g.
        // `pdf.set_object(holder, Object::Reference(target))` to redirect a
        // holder chain in place, exactly as `ref_chain.rs`'s own test
        // fixture does. `resolve_object_handle` itself can never produce
        // this state (a top-level bare reference never comes from a
        // file/ObjStm parse — `parser.rs`'s `top_level_no_reference`
        // integerizes it instead, matching qpdf — and `set_object` always
        // resolves the same canonical handle it writes into the legacy
        // cache, so `resolve_object_handle`'s own `is_resolved` early-return
        // guards against ever re-deriving this value itself), but the state
        // is still reachable from any public accessor call on the handle
        // `Pdf::set_object` resolved directly. This test calls the same
        // `set_resolved` method `set_object` itself calls, to exercise the
        // state without pulling `Pdf` into this single-file slice.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));
        assert_eq!(handle.type_code(), 13, "ot_unresolved");
        assert_eq!(handle.type_name(), "unresolved");
        // The contradiction this method's own doc calls out: the value
        // itself is known (is_resolved() is true) even though its type
        // code reports the same ordinal as a handle whose value is not
        // known at all.
        assert!(handle.is_resolved());
    }
}

#[cfg(test)]
mod unparse_tests {
    use super::*;

    #[test]
    fn unparse_resolved_covers_every_teardown_arm_for_a_nested_array_dict_and_stream() {
        // Codex Review on PR #603 (discussion_r3689896128) found that the
        // recursive materialization walk backing unparse/unparse_resolved
        // had no protection against a deeply nested tree's per-frame stack
        // cost. Two recursion points were fixed: construction
        // (unparse_materialize, wrapped in stacker::maybe_grow) and the
        // resulting materialized `Object` tree's own teardown right after
        // (unparse_drop_iteratively, an explicit-stack walk replacing
        // Object's ordinary recursive Drop).
        //
        // This test intentionally does NOT probe an extreme depth. A
        // depth large enough to discriminate "protected" from
        // "unprotected" on every CI runner turned out not to exist: a
        // depth safe on this author's local machine (4,000) still
        // stack-overflowed on macOS/ARM/Windows CI runners, because
        // `Object::write_pdf` -- called on the materialized tree between
        // the two now-protected walks -- is *itself* an unprotected
        // recursive serializer living in object.rs, outside this slice's
        // file allowlist (only object_handle.rs may change). Fixing that
        // third recursion point is out of scope here; it is tracked
        // together with the other pre-existing unprotected-recursion
        // concerns in flpdf-egzr.3.5. Until it lands, no caller of
        // unparse_resolved has arbitrary-depth safety, so this test
        // stays at a shallow, portable depth and exercises every
        // container arm (Array, Dictionary, Stream) that
        // unparse_drop_iteratively and drain_dictionary_onto handle,
        // rather than chasing a depth number.
        let stream = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        stream.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"Length".to_vec(),
                ObjectHandle::integer(0),
            )]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            stream_length: 0,
        });
        let inner_dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::null()),
            (b"B".to_vec(), ObjectHandle::integer(2)),
        ]);
        let array = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            inner_dict,
            stream,
            ObjectHandle::array(vec![ObjectHandle::name(b"Nested".to_vec())]),
        ]);

        assert_eq!(
            array.unparse_resolved(),
            b"[ 1 << /B 2 >> 9 0 R [ /Nested ] ]"
        );
    }

    #[test]
    fn direct_scalar_unparses_like_object_write_pdf() {
        assert_eq!(ObjectHandle::integer(7).unparse(), b"7");
        assert_eq!(ObjectHandle::boolean(true).unparse(), b"true");
        assert_eq!(ObjectHandle::name(b"Type".to_vec()).unparse(), b"/Type");
    }

    #[test]
    fn indirect_handle_unparse_is_always_the_reference_form_even_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        assert_eq!(handle.unparse(), b"7 2 R");
    }

    #[test]
    fn indirect_handle_unparse_resolved_falls_back_to_null_before_resolution() {
        // No hidden I/O: an unresolved indirect handle's value is not
        // known, so unparse_resolved reports the same as materialize()'s
        // own documented null fallback rather than triggering resolution.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        assert_eq!(handle.unparse_resolved(), b"null");
    }

    #[test]
    fn resolved_indirect_handle_unparse_resolved_shows_the_real_value() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        handle.set_resolved(ObjectValue::Integer(42));
        assert_eq!(handle.unparse(), b"7 2 R");
        assert_eq!(handle.unparse_resolved(), b"42");
    }

    #[test]
    fn stream_value_unparse_resolved_still_reports_the_reference_form() {
        // QPDF_Stream::unparse() (libqpdf/QPDF_Stream.cc:173-178) always
        // returns its own "N G R" — mirrored here rather than inlining the
        // stream's dictionary/data.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(0))]);
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        handle.set_resolved(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            stream_length: 0,
        });
        assert_eq!(handle.unparse(), b"9 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
    }

    #[test]
    fn direct_array_unparse_writes_indirect_children_as_references_not_recursed() {
        let child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), 0);
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), child]);
        assert_eq!(array.unparse(), b"[ 1 5 0 R ]");
    }

    #[test]
    fn a_direct_stream_value_unparse_resolved_inlines_rather_than_referencing() {
        // A *direct* Stream `ObjectValue` is not the common case (no public
        // `ObjectHandle::stream(..)` factory exists; production reader code
        // installs real stream values via `set_resolved` on an indirect
        // handle), but it IS reachable through the public API: a nested
        // `Object::Stream` passed to `Pdf::set_object` (e.g. inside an
        // `Object::Array`) is lifted via `reader.rs`'s `lift_bounded`'s
        // direct-value arm into `ObjectHandle::from_value`, producing
        // exactly this shape. Real qpdf has no equivalent state -- a stream
        // is only ever a *newly allocated indirect* `QPDFObjectHandle`
        // (`QPDFObjectHandle::newStream`) -- so `QPDF_Stream::unparse()`'s
        // reference-form guarantee has nothing to say about this case, and
        // there is no qpdf byte-parity oracle to match here. This test
        // pins down that the fallback path (materialize + `Object::write_pdf`)
        // handles this shape by inlining the dictionary and data the same
        // way `Object::write_pdf` already does for `Object::Stream`, rather
        // than fabricating a meaningless reference for a value that was
        // never assigned an object number/generation.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        assert_eq!(
            handle.unparse_resolved(),
            b"<< /Length 2 >>\nstream\nab\nendstream"
        );
    }

    #[test]
    fn destroyed_direct_handle_unparse_and_unparse_resolved_fall_back_to_null() {
        // Disconnect clears the shared slot's indirect metadata, so both
        // entry points reach the existing infallible fallback for the
        // `Destroyed` value. qpdf's `QPDF_Destroyed::unparse()`
        // (`libqpdf/QPDF_Destroyed.cc:24-29`) throws `std::logic_error`, but
        // neither method has an exception channel to mirror that with.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.disconnect();

        assert_eq!(handle.unparse(), b"null");
        assert_eq!(handle.unparse_resolved(), b"null");
    }

    #[test]
    fn resolved_to_a_reference_indirect_handle_unparse_and_unparse_resolved_diverge() {
        // Mirrors `type_code_tests::resolved_to_a_reference_indirect_handle_reports_unresolved`'s
        // own state: a handle `Pdf::set_object` redirected in place to
        // another object (see `ObjectValue::Reference`'s own doc).
        // `unparse()` never dereferences an indirect handle (see this
        // module's `destroyed_...` test above), so it reports the
        // redirecting handle's own "N G R" -- not the target's.
        // `unparse_resolved()` does read the resolved value, which is
        // itself a bare reference, so it reports the *target's* "N G R"
        // instead of chasing to the target's own concrete value (e.g.
        // `42`). This is a real gap, not a documented design choice: this
        // crate's own `flpdf-qtest-tools::driver::Handle` already has an
        // established, tested contract for exactly this redirect scenario
        // (`reference_chain_resolves_but_unparse_retains_the_first_reference`,
        // `driver/handle.rs:678-696`) where the equivalent accessor *does*
        // chase to the target's terminal value while `unparse()` keeps
        // reporting the first reference's own identity -- this method does
        // not yet replicate that. Chasing needs `Pdf` (`ObjectValue::Reference`
        // stores only a bare `ObjectRef`, not a handle link), so it cannot be
        // implemented from this file alone; tracked as flpdf-l3kz, and wired
        // as a hard dependency of flpdf-egzr.3.2.3 (the slice that migrates
        // `driver::Handle` itself onto this API and must not regress that
        // test).
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));

        assert_eq!(handle.unparse(), b"1 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
    }

    #[test]
    fn unparse_resolved_omits_a_direct_null_dictionary_entry() {
        // qpdf's QPDF_Dictionary::unparse() (libqpdf/QPDF_Dictionary.cc:59-69)
        // skips any entry whose value isNull(), matching the PDF-spec
        // equivalence between an explicit null value and a missing key.
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::null()),
            (b"B".to_vec(), ObjectHandle::integer(1)),
        ]);
        assert_eq!(dict.unparse_resolved(), b"<< /B 1 >>");
    }

    #[test]
    fn unparse_resolved_omits_an_already_resolved_null_indirect_dictionary_entry() {
        // The same qpdf rule applies to an indirect child, since qpdf's
        // isNull() dereferences before checking -- but only when this
        // child's value is already known (is_resolved()), never by forcing
        // new resolution (see the "keeps a not-yet-resolved" test below).
        let missing = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        missing.set_missing();
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), missing),
            (b"B".to_vec(), ObjectHandle::integer(1)),
        ]);
        assert_eq!(dict.unparse_resolved(), b"<< /B 1 >>");
    }

    #[test]
    fn unparse_resolved_keeps_a_not_yet_resolved_indirect_dictionary_entry() {
        // Divergence from qpdf, which would resolve the child to check its
        // nullness (QPDFObjectHandle::isNull() dereferences,
        // libqpdf/QPDFObjectHandle.cc:353-356); this port never performs
        // hidden resolution (see unparse_resolved's own doc), so an entry
        // whose nullness is not yet known is conservatively kept rather
        // than guessed away.
        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), unresolved)]);
        assert_eq!(dict.unparse_resolved(), b"<< /A 9 0 R >>");
    }

    #[test]
    fn unparse_resolved_omits_nulls_in_a_nested_dictionary_inside_an_array() {
        let inner = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        let array = ObjectHandle::array(vec![inner]);
        assert_eq!(array.unparse_resolved(), b"[ << >> ]");
    }

    #[test]
    fn unparse_resolved_does_not_omit_null_array_elements() {
        // Only dictionary keys are omitted for a null value; array elements
        // keep their position (QPDF_Array::unparse(),
        // libqpdf/QPDF_Array.cc:123-140, explicitly fills gaps with the
        // literal "null" token rather than skipping them).
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::null()]);
        assert_eq!(array.unparse_resolved(), b"[ 1 null ]");
    }
}

#[cfg(test)]
mod unparse_object_tests {
    use super::identity_tests::{error_resolving_handle, resolver_bearing_handle};
    use super::*;

    #[test]
    fn visible_dict_entries_keeps_non_null_and_drops_direct_null() {
        let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![
            (b"Zulu".to_vec(), ObjectHandle::integer(26)),
            (b"DirectNull".to_vec(), ObjectHandle::null()),
        ];
        let visible = visible_dict_entries(&entries).expect("no resolver needed");
        let keys: Vec<&[u8]> = visible.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, [b"Zulu".as_slice()]);
    }

    #[test]
    fn visible_dict_entries_resolves_and_drops_an_indirect_null() {
        let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
        let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![(b"RefNull".to_vec(), indirect_null)];
        let visible = visible_dict_entries(&entries).unwrap();
        assert!(visible.is_empty());
    }

    #[test]
    fn visible_dict_entries_propagates_a_dropped_document_error() {
        let (indirect_null, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let entries: Vec<(Vec<u8>, ObjectHandle)> = vec![(b"RefNull".to_vec(), indirect_null)];
        assert!(visible_dict_entries(&entries).is_err());
    }

    #[test]
    fn dict_is_sig_with_byte_range_true_when_both_present() {
        let entries = vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
        ];
        assert!(dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_false_when_byte_range_is_a_direct_null() {
        // `QPDF_Dictionary::hasKey` (QPDF_Dictionary.cc:98-101) is
        // `items.count(key) > 0 && !items[key].isNull()` -- a null
        // `/ByteRange` value counts as *absent*, the same way a null-valued
        // entry is excluded from `visible_dict_entries`'s own output.
        let entries = vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"/ByteRange".to_vec(), ObjectHandle::null()),
        ];
        assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_false_when_byte_range_resolves_to_an_indirect_null() {
        // Same null-exclusion rule as the direct case above, but through
        // `isNull()`'s own dereference (QPDFObjectHandle.cc:353-356) --
        // `hasKey` resolves an indirect `/ByteRange` to decide this too.
        let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
        let entries = vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"/ByteRange".to_vec(), indirect_null),
        ];
        assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_propagates_a_dropped_document_error_from_byte_range() {
        // `/Type` is a direct `/Sig`, so the short-circuit passes it and
        // reaches `/ByteRange`'s own forced resolution, which must
        // propagate a dropped-document error the same way `/Type`'s own
        // resolution does.
        let (indirect_byte_range, resolver) = error_resolving_handle(ObjectRef::new(32, 0));
        drop(resolver);
        let entries = vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"/ByteRange".to_vec(), indirect_byte_range),
        ];
        assert!(dict_is_sig_with_byte_range(&entries).is_err());
    }

    #[test]
    fn dict_is_sig_with_byte_range_false_without_byte_range_key() {
        let entries = vec![(b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec()))];
        assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_false_without_type_key() {
        let entries = vec![(b"/ByteRange".to_vec(), ObjectHandle::array(vec![]))];
        assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_false_when_type_is_not_sig() {
        let entries = vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
        ];
        assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_resolves_an_indirect_type_that_is_sig() {
        // Mirrors qpdf's own `getKey("/Type").isNameAndEquals("/Sig")`,
        // which dereferences through `isName()` -- an indirect `/Type` must
        // be force-resolved to decide this, not conservatively treated as
        // "not Sig".
        let (indirect_type, _resolver) =
            resolver_bearing_handle(ObjectValue::Name(b"Sig".to_vec()));
        let entries = vec![
            (b"/Type".to_vec(), indirect_type),
            (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
        ];
        assert!(dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn dict_is_sig_with_byte_range_propagates_a_dropped_document_error_from_type() {
        let (indirect_type, resolver) = error_resolving_handle(ObjectRef::new(30, 0));
        drop(resolver);
        let entries = vec![
            (b"/Type".to_vec(), indirect_type),
            (b"/ByteRange".to_vec(), ObjectHandle::array(vec![])),
        ];
        assert!(dict_is_sig_with_byte_range(&entries).is_err());
    }

    #[test]
    fn dict_is_sig_with_byte_range_does_not_resolve_byte_range_when_type_is_not_sig() {
        // Mirrors qpdf's own `&&` short-circuit
        // (`isDictionaryOfType("/Sig") && hasKey("/ByteRange")`,
        // QPDFWriter.cc:1497-1498): `/ByteRange`'s resolver must never run
        // at all once `/Type` is confirmed not `/Sig` -- an indirect
        // `/ByteRange` whose resolver would error must not surface that
        // error here.
        let (indirect_byte_range, _resolver) = error_resolving_handle(ObjectRef::new(31, 0));
        let entries = vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"/ByteRange".to_vec(), indirect_byte_range),
        ];
        assert!(!dict_is_sig_with_byte_range(&entries).unwrap());
    }

    #[test]
    fn unparse_object_writes_sig_contents_as_a_hex_string() {
        // `/Contents` is a printable-ASCII direct String, which the ordinary
        // `write_string_value` path (via `use_hex_string`) would write as a
        // literal string `(hi)` -- confirms the Sig+ByteRange special case
        // overrides that choice with `write_hex_string`'s own byte shape
        // (`<` + one lowercase hex pair per byte + `>`: `h` = 0x68, `i` =
        // 0x69).
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        ]);
        let mut out = Vec::new();
        dict.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /ByteRange [ ] /Contents <6869> /Type /Sig >>");
    }

    #[test]
    fn unparse_object_leaves_contents_literal_without_sig_type() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        ]);
        let mut out = Vec::new();
        dict.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /ByteRange [ ] /Contents (hi) /Type /Page >>");
    }

    #[test]
    fn unparse_object_leaves_contents_literal_without_byte_range() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        ]);
        let mut out = Vec::new();
        dict.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /Contents (hi) /Type /Sig >>");
    }

    #[test]
    fn unparse_object_writes_an_indirect_sig_contents_as_reference_form_not_hex() {
        // Mirrors `unparseChild`'s own indirect-first short-circuit
        // (QPDFWriter.cc:1149-1156): an indirect `/Contents` value writes
        // as its own "N G R" reference form regardless of the Sig+ByteRange
        // condition -- qpdf's flags are only consulted inside
        // `unparseObject`, which an indirect child never reaches.
        let (indirect_contents, _resolver) =
            resolver_bearing_handle(ObjectValue::String(b"hi".to_vec()));
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), indirect_contents),
        ]);
        let mut out = Vec::new();
        dict.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /ByteRange [ ] /Contents 20 0 R /Type /Sig >>");
    }

    #[test]
    fn unparse_object_leaves_a_non_string_sig_contents_unaffected() {
        // The Sig+ByteRange special case only affects a child whose
        // resolved value is itself a String (QPDFWriter.cc's `f_hex_string`
        // handling lives inside the `ot_string` arm alone) -- a non-String
        // direct `/Contents` (unusual in practice, but not structurally
        // ruled out) falls through to the ordinary child-writer unaffected.
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::integer(7)),
        ]);
        let mut out = Vec::new();
        dict.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /ByteRange [ ] /Contents 7 /Type /Sig >>");
    }

    #[test]
    fn unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
        // Code-quality review of commit 6cae41fd: `dict_is_sig_with_byte_range`
        // was hoisted to run once, unconditionally, before the per-entry loop
        // even started -- rather than lazily, only when the loop's own
        // iteration actually reaches a surviving `/Contents` key -- for the
        // Sig+ByteRange hex-string special case that same commit added. This
        // reintroduced the identical unnecessary-force-resolution bug class
        // Finding 2's `refiltered`-key exclusion fix (see
        // `unparse_stream_dict_entries`'s own doc) already fixed for
        // `/Filter`/`/DecodeParms`: `/Type` (and, conditionally,
        // `/ByteRange`) now gets force-resolved even for a dict with no
        // `/Contents` key at all to apply the special case to -- see
        // `try_write_sig_contents_hex_string`'s own doc for why the operand
        // order matters, not just the call site's existence.
        //
        // This dict has no `/Contents` key, so a correctly lazy
        // implementation never even calls `dict_is_sig_with_byte_range` --
        // `/Type`'s own resolution must be driven solely by
        // `visible_dict_entries`'s ordinary per-key null-suppression pass,
        // which runs in the dict's own (`BTreeMap`) alphabetical key order.
        // `/AAA` sorts before `/Type`; both are dropped-document handles at
        // distinct object refs, so the surfaced error text -- which embeds
        // the failing ref, see
        // `try_dereference_reports_a_dropped_document_without_reconnecting`
        // -- pins exactly which one was actually touched first: with the
        // eager bug, `/Type` (object 30) is resolved before the
        // null-suppression loop even starts, so its error surfaces even
        // though `/AAA` (object 99) sorts first in qpdf's own single-pass
        // loop order; with the fix, `/AAA`'s error surfaces instead, and
        // `/Type`'s handle is never dereferenced at all. (A plain
        // success-vs-error assertion cannot discriminate this bug on its
        // own: `visible_dict_entries` itself already force-resolves every
        // surviving entry -- including `/Type`/`/ByteRange` whenever they
        // are present as dict keys -- for the ordinary null-suppression
        // check, matching qpdf's own `isNull()` call inside the identical
        // per-item loop, `QPDFWriter.cc:1488-1491`; a dict with a genuinely
        // erroring `/Type` errors either way. The bug is about *which*
        // resolution happens first among several, not whether the overall
        // call succeeds.)
        let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
        drop(aaa_resolver);
        let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
        drop(type_resolver);
        let dict = ObjectHandle::dictionary(vec![
            (b"AAA".to_vec(), aaa),
            (b"Type".to_vec(), sig_type),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        ]);
        let mut out = Vec::new();
        let error = dict.unparse_object(&mut out).unwrap_err();
        assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
    }

    #[test]
    fn unparse_object_qdf_writes_sig_contents_as_a_hex_string() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        ]);
        let mut out = Vec::new();
        dict.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(
            out,
            b"<<\n  /ByteRange [\n  ]\n  /Contents <6869>\n  /Type /Sig\n>>"
        );
    }

    #[test]
    fn unparse_object_qdf_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
        // QDF sibling of
        // `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
        // above -- `unparse_dict_entries_qdf` needed the identical fix at
        // its own call site. See that test's own doc for the full
        // eager-vs-lazy rationale and why a plain success-vs-error
        // assertion cannot discriminate this bug.
        let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
        drop(aaa_resolver);
        let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
        drop(type_resolver);
        let dict = ObjectHandle::dictionary(vec![
            (b"AAA".to_vec(), aaa),
            (b"Type".to_vec(), sig_type),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
        ]);
        let mut out = Vec::new();
        let error = dict.unparse_object_qdf(&mut out, 0).unwrap_err();
        assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
    }

    #[test]
    fn unparse_stream_body_writes_sig_contents_as_a_hex_string() {
        // The Sig+ByteRange special case has no `f_stream` guard in real
        // qpdf either (QPDFWriter.cc:1490-1504 is the same shared loop the
        // stream-dictionary branch falls into) -- a stream whose dict
        // happens to be `/Type /Sig` with `/ByteRange` is unusual but not
        // structurally ruled out.
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
            (b"Length".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(
            out,
            b"<< /ByteRange [ ] /Contents <6869> /Type /Sig /Length 2 >>"
        );
    }

    #[test]
    fn unparse_stream_body_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached() {
        // Stream sibling of
        // `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
        // above -- `unparse_stream_dict_entries` needed the identical fix at
        // its own call site. See that test's own doc for the full
        // eager-vs-lazy rationale and why a plain success-vs-error
        // assertion cannot discriminate this bug. `/Length` is a harmless
        // direct value here -- this test is not about the `refiltered`
        // dimension Finding 2 already fixed, so `refiltered == false`.
        let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
        drop(aaa_resolver);
        let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
        drop(type_resolver);
        let dict = ObjectHandle::dictionary(vec![
            (b"AAA".to_vec(), aaa),
            (b"Type".to_vec(), sig_type),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Length".to_vec(), ObjectHandle::integer(0)),
        ]);
        let mut out = Vec::new();
        let error = dict.unparse_stream_body(&mut out, false).unwrap_err();
        assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
    }

    #[test]
    fn write_child_writes_indirect_handle_as_reference_form() {
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let mut out = Vec::new();
        write_child(&indirect, &mut out).unwrap();
        assert_eq!(out, b"20 0 R");
    }

    #[test]
    fn write_child_recurses_into_a_direct_scalar() {
        let mut out = Vec::new();
        write_child(&ObjectHandle::integer(7), &mut out).unwrap();
        assert_eq!(out, b"7");
    }

    #[test]
    fn unparse_object_writes_a_scalar() {
        let mut out = Vec::new();
        ObjectHandle::integer(42).unparse_object(&mut out).unwrap();
        assert_eq!(out, b"42");
    }

    #[test]
    fn unparse_object_writes_a_boolean() {
        let mut out = Vec::new();
        ObjectHandle::boolean(true)
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"true");
        out.clear();
        ObjectHandle::boolean(false)
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"false");
    }

    #[test]
    fn unparse_object_writes_a_real() {
        let mut out = Vec::new();
        ObjectHandle::real(0.5).unparse_object(&mut out).unwrap();
        assert_eq!(out, b"0.5");
    }

    #[test]
    fn unparse_object_writes_a_string() {
        let mut out = Vec::new();
        ObjectHandle::string(b"hi".to_vec())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"(hi)");
    }

    #[test]
    fn unparse_object_writes_an_operator_verbatim() {
        // ObjectValue::InlineImage shares the identical match arm and byte
        // path, so this one case covers both bindings.
        let mut out = Vec::new();
        ObjectHandle::operator(b"q".to_vec())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"q");
    }

    #[test]
    fn unparse_object_serializes_large_scalar_payloads_without_deep_snapshotting() {
        let string_payload = vec![b's'; 256 * 1024];
        let mut out = Vec::new();
        ObjectHandle::string(string_payload.clone())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out.len(), string_payload.len() + 2);
        assert_eq!(out.first(), Some(&b'('));
        assert_eq!(out.last(), Some(&b')'));

        let operator_payload = vec![b'o'; 256 * 1024];
        out.clear();
        ObjectHandle::operator(operator_payload.clone())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, operator_payload);

        let inline_image_payload = vec![b'i'; 256 * 1024];
        out.clear();
        ObjectHandle::inline_image(inline_image_payload.clone())
            .unparse_object_qdf(&mut out, 0)
            .unwrap();
        assert_eq!(out, inline_image_payload);
    }

    #[test]
    fn unparse_object_inlines_only_the_dictionary_of_a_direct_stream_value() {
        // A *direct* Stream ObjectValue has no qpdf counterpart (a real
        // QPDFObjectHandle's resolved value is never itself a stream
        // outside an indirect object), so there is no byte-parity oracle
        // here. This pins down the same "inline the dictionary, do not
        // write the `stream`/`endstream` framing" behavior `unparse_stream_body`
        // (Task 6) is separately responsible for, and stays consistent with
        // that primitive's scope rather than reproducing framing logic here.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary() {
        // Unlike the direct-stream case above, this *is* a real, reachable
        // qpdf shape: an indirect object whose resolved value is a stream.
        // `unparse_object`/`unparse_object_walk` dispatch on `self` directly
        // (never through `write_child`'s indirect-reference short-circuit,
        // which only applies to *child* positions during recursion), so
        // this reaches the same `ObjectValue::Stream` arm as the direct
        // case and inlines just the dictionary -- not qpdf's real
        // stream-writing output at this position (see
        // `ObjectHandle::unparse_object`'s own doc). Pins today's actual
        // behavior; `unparse_stream_body` (Task 6) is the primitive that
        // will implement the real stream-writing path.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_object_writes_a_resolved_indirect_reference_value_as_the_targets_own_form() {
        // ObjectValue::Reference is a real, reachable resolution state (a
        // `Pdf::set_object` redirect -- see its own doc and
        // `unparse_tests::resolved_to_a_reference_indirect_handle_unparse_and_unparse_resolved_diverge`,
        // which builds the identical shape). No qpdf counterpart exists, so
        // this mirrors `unparse_resolved`'s own choice for it rather than
        // writing nothing.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"9 0 R");
    }

    #[test]
    fn unparse_object_writes_a_name_escaped() {
        let mut out = Vec::new();
        ObjectHandle::name(b"application/pdf".to_vec())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"/application#2fpdf");
    }

    #[test]
    fn unparse_object_writes_a_real_literal_when_safe() {
        let mut out = Vec::new();
        ObjectHandle::real_literal(0.4, b".4".to_vec())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b".4");
    }

    #[test]
    fn unparse_object_falls_back_to_canonical_when_literal_is_unsafe() {
        let mut out = Vec::new();
        ObjectHandle::real_literal(0.4, b"nope".to_vec())
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"0.4");
    }

    #[test]
    fn unparse_object_writes_an_array_with_qpdf_spacing() {
        let handle = ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(2)]);
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"[ 1 2 ]");
    }

    #[test]
    fn unparse_object_writes_an_empty_array() {
        let mut out = Vec::new();
        ObjectHandle::array(vec![])
            .unparse_object(&mut out)
            .unwrap();
        assert_eq!(out, b"[ ]");
    }

    #[test]
    fn unparse_object_writes_a_dict_and_suppresses_direct_null() {
        let handle = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"B".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /A 1 >>");
    }

    #[test]
    fn unparse_object_suppresses_an_indirect_entry_resolving_to_null() {
        let (indirect_null, _resolver) = resolver_bearing_handle(ObjectValue::Null);
        let handle = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"RefNull".to_vec(), indirect_null),
        ]);
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /A 1 >>");
    }

    #[test]
    fn unparse_object_writes_an_empty_dict_when_every_entry_is_suppressed() {
        let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< >>");
    }

    #[test]
    fn unparse_object_writes_a_retained_indirect_entry_as_reference_form() {
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), indirect)]);
        let mut out = Vec::new();
        handle.unparse_object(&mut out).unwrap();
        assert_eq!(out, b"<< /A 20 0 R >>");
    }

    #[test]
    fn unparse_object_propagates_a_dropped_document_error() {
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.unparse_object(&mut out).is_err());
    }

    #[test]
    fn unparse_object_qdf_writes_a_scalar_like_plain_unparse() {
        let mut out = Vec::new();
        ObjectHandle::integer(42)
            .unparse_object_qdf(&mut out, 0)
            .unwrap();
        assert_eq!(out, b"42");
    }

    #[test]
    fn unparse_object_qdf_writes_an_array_with_newline_indent() {
        let handle = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"[\n  1\n]");
    }

    #[test]
    fn unparse_object_qdf_writes_a_dict_with_newline_indent_and_suppresses_null() {
        let handle = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"B".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /A 1\n>>");
    }

    #[test]
    fn unparse_object_qdf_nests_indent_one_level_deeper() {
        let handle = ObjectHandle::dictionary(vec![(
            b"Kids".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(1)]),
        )]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /Kids [\n    1\n  ]\n>>");
    }

    #[test]
    fn unparse_object_qdf_writes_a_retained_indirect_entry_as_reference_form() {
        // QDF-mode sibling of unparse_object_writes_a_retained_indirect_entry_as_reference_form:
        // exercises write_child_qdf's indirect arm, which the four
        // plan-specified literals above never reach (every handle in them is
        // direct).
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), indirect)]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /A 20 0 R\n>>");
    }

    #[test]
    fn unparse_object_qdf_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary() {
        // QDF-mode sibling of
        // unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary:
        // exercises unparse_object_value_qdf's Stream arm, unreached by the
        // four plan-specified literals above.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /Length 2\n>>");
    }

    #[test]
    fn unparse_object_qdf_nests_a_dict_inside_a_dict_at_every_indent_slot() {
        // The plan-specified nesting literal only stretches the *array*
        // closing bracket's indent slot (unparse_object_qdf_nests_indent_one_level_deeper).
        // A dict nested in a dict stretches unparse_dict_entries_qdf's own
        // closing `>>` indent slot too, which that test leaves at the
        // top-level `indent = 0` default.
        let handle = ObjectHandle::dictionary(vec![(
            b"D".to_vec(),
            ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]),
        )]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /D <<\n    /A 1\n  >>\n>>");
    }

    #[test]
    fn unparse_object_qdf_propagates_a_dropped_document_error() {
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.unparse_object_qdf(&mut out, 0).is_err());
    }

    #[test]
    fn unparse_object_qdf_respects_a_nonzero_starting_indent() {
        // Every other QDF test in this module calls the public entry point
        // with `indent = 0`, so the internal `indent + 2` recursion is
        // exercised but the *argument's own arrival* at the public method
        // never is -- a stray `indent = 0` hardcoded inside the function
        // body would still pass every one of them. Start at a nonzero
        // column instead, so the dict's closing `>>` (written at the
        // caller's own `indent`, unincremented) and its one entry (written
        // at `indent + 2`) both prove the argument actually reached them.
        let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 4).unwrap();
        assert_eq!(out, b"<<\n      /A 1\n    >>");
    }

    #[test]
    fn unparse_object_qdf_writes_an_empty_dict_at_a_nonzero_indent() {
        // Sibling of the suppresses-null test above, but with *every* entry
        // gone (here: none to begin with) and at a nonzero starting indent
        // -- untested at any indent before this. Only the closing `>>`
        // carries an indent slot when there are no surviving entries; this
        // pins that it still lands at the caller's own `indent`, not the
        // default `0`.
        let handle = ObjectHandle::dictionary(vec![]);
        let mut out = Vec::new();
        handle.unparse_object_qdf(&mut out, 4).unwrap();
        assert_eq!(out, b"<<\n    >>");
    }

    #[test]
    fn unparse_stream_body_writes_length_last_preserved() {
        // `/DecodeParms` is a non-null (empty-dictionary) value here, not
        // `ObjectHandle::null()`: a null value would already be excluded by
        // `visible_dict_entries`'s own null-suppression pass before the
        // `refiltered` check ever saw the key, which would let this test
        // pass even if `unparse_stream_dict_entries` unconditionally
        // dropped `/DecodeParms` regardless of `refiltered`. With
        // `refiltered == false`, both `/DecodeParms` and `/Filter` must
        // survive at their natural (`BTreeMap`) lexicographic positions
        // (`DecodeParms` < `Filter` < the pulled-out-and-appended
        // `Length`), unlike the refiltered case pinned by
        // `unparse_stream_body_refiltered_drops_filter_and_decodeparms_appends_flate`
        // below, where both are dropped.
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(
            out,
            b"<< /DecodeParms << >> /Filter /FlateDecode /Length 3 >>"
        );
    }

    #[test]
    fn unparse_stream_body_refiltered_drops_filter_and_decodeparms_appends_flate() {
        // `/DecodeParms` is a non-null (empty-dictionary) value here, not
        // `ObjectHandle::null()`: a null value would already be excluded by
        // `visible_dict_entries`'s own null-suppression pass before the
        // refiltered check ever saw the key, which would let this test pass
        // even if the `key.as_slice() == b"DecodeParms"` disjunct were
        // dropped from that check entirely.
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            ),
            (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body(&mut out, true).unwrap();
        assert_eq!(out, b"<< /Length 3 /Filter /FlateDecode >>");
    }

    #[test]
    fn unparse_stream_body_refiltered_does_not_resolve_a_would_error_indirect_filter_or_decodeparms(
    ) {
        // Codex Review on PR #644 (crates/flpdf/src/object_handle.rs:2425):
        // `visible_dict_entries` previously ran over every entry --
        // including `/Filter`/`/DecodeParms` -- before the `refiltered`
        // skip in the write loop below ever got a chance to drop them,
        // force-resolving (and potentially failing on) a value guaranteed
        // to be discarded from the refiltered output. Real qpdf removes
        // both keys from a shallow copy of the dict entirely BEFORE its
        // null-suppression loop runs (`QPDFWriter.cc:1454-1455`, ahead of
        // `:1488-1491`) -- it never calls `isNull()` on a key it is about
        // to discard anyway.
        //
        // An indirect `/Filter`/`/DecodeParms` whose resolver would error,
        // with `refiltered == true`, must now succeed without ever calling
        // that resolver -- proving the values are excluded before
        // suppression ever inspects them, not merely skipped during the
        // write loop after being resolved (which would have propagated
        // `ErrorResolver`'s error through `visible_dict_entries`'s
        // `try_is_null()` call before this fix).
        let (filter, _filter_resolver) = error_resolving_handle(ObjectRef::new(40, 0));
        let (decode_parms, _decode_parms_resolver) = error_resolving_handle(ObjectRef::new(41, 0));
        let dict = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), filter),
            (b"DecodeParms".to_vec(), decode_parms),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body(&mut out, true).unwrap();
        assert_eq!(out, b"<< /Length 3 /Filter /FlateDecode >>");
    }

    #[test]
    fn unparse_stream_body_not_refiltered_still_propagates_an_indirect_filter_error() {
        // Contrast with the test above: when `refiltered` is false,
        // `/Filter` is a surviving key that must actually be written, so an
        // indirect value whose resolver errors must still surface that
        // error -- proving the fix scopes "skip resolution" to the
        // refiltered case alone, rather than exempting
        // `/Filter`/`/DecodeParms` from resolution unconditionally (which
        // would silently corrupt a non-refiltered stream's real `/Filter`
        // value).
        let (filter, _resolver) = error_resolving_handle(ObjectRef::new(42, 0));
        let dict = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), filter),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let mut out = Vec::new();
        assert!(dict.unparse_stream_body(&mut out, false).is_err());
    }

    #[test]
    fn mapped_unparse_writes_stream_children_and_non_dictionary_shapes() {
        let (child, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let dict = ObjectHandle::dictionary(vec![
            (b"Child".to_vec(), child),
            (b"Length".to_vec(), ObjectHandle::integer(2)),
        ]);
        let stream = ObjectHandle::stream(dict, Rc::new(b"ab".to_vec()));
        let map = |object_ref| {
            assert_eq!(object_ref, ObjectRef::new(20, 0));
            Ok(ObjectRef::new(8, 0))
        };

        let mut object = Vec::new();
        stream
            .unparse_object_with_ref_map(&mut object, &map)
            .unwrap();
        assert_eq!(object, b"<< /Child 8 0 R /Length 2 >>");

        let mut body = Vec::new();
        stream
            .unparse_stream_body_with_ref_map(&mut body, false, &map)
            .unwrap();
        assert_eq!(body, b"<< /Child 8 0 R /Length 2 >>");

        let signature = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
                (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
                (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
            ]),
            Rc::new(b"ab".to_vec()),
        );
        let mut signature_body = Vec::new();
        signature
            .unparse_stream_body_with_ref_map(&mut signature_body, false, &map)
            .unwrap();
        assert_eq!(
            signature_body,
            b"<< /ByteRange [ ] /Contents <6869> /Type /Sig >>"
        );

        let mut nested_non_dictionary = Vec::new();
        ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()))
            .unparse_stream_body_with_ref_map(&mut nested_non_dictionary, false, &map)
            .unwrap();
        assert_eq!(nested_non_dictionary, b"<< >>");

        let mut non_dictionary = Vec::new();
        ObjectHandle::integer(5)
            .unparse_stream_body_with_ref_map(&mut non_dictionary, false, &map)
            .unwrap();
        assert_eq!(non_dictionary, b"<< >>");
    }

    #[test]
    fn mapped_unparse_releases_reference_value_borrow_before_mapping() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Reference(ObjectRef::new(2, 0)));
        let reentrant_handle = handle.clone();
        let map = move |object_ref: ObjectRef| {
            reentrant_handle.set_resolved(ObjectValue::Integer(7));
            Ok(ObjectRef::new(
                object_ref.number + 10,
                object_ref.generation,
            ))
        };

        let mut out = Vec::new();
        handle
            .unparse_object_with_ref_map(&mut out, &map)
            .expect("reference mapping may re-enter the handle");

        assert_eq!(out, b"12 0 R");
        assert_eq!(handle.as_integer(), Some(7));
    }

    #[test]
    fn mapped_unparse_omits_removed_indirect_dictionary_entries() {
        let removed_ref = ObjectRef::new(20, 0);
        let (removed, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let kept = ObjectHandle::new_indirect_unresolved(ObjectRef::new(21, 0), 0);
        kept.set_resolved(ObjectValue::Integer(8));
        let mut removed_refs = BTreeSet::new();
        removed_refs.insert(removed_ref);
        let map = |object_ref| {
            assert_ne!(object_ref, removed_ref);
            Ok(ObjectRef::new(8, 0))
        };

        let dict = ObjectHandle::dictionary(vec![
            (b"Mapped".to_vec(), kept.clone()),
            (b"Removed".to_vec(), removed.clone()),
        ]);
        let mut object = Vec::new();
        dict.unparse_object_with_ref_map_and_removed(&mut object, &map, &removed_refs)
            .unwrap();
        assert_eq!(object, b"<< /Mapped 8 0 R >>");

        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"Mapped".to_vec(), kept),
                (b"Length".to_vec(), ObjectHandle::integer(2)),
                (b"Removed".to_vec(), removed),
            ]),
            Rc::new(b"ab".to_vec()),
        );
        let mut body = Vec::new();
        stream
            .unparse_stream_body_with_ref_map_and_removed(&mut body, false, &map, &removed_refs)
            .unwrap();
        assert_eq!(body, b"<< /Mapped 8 0 R /Length 2 >>");
    }

    #[test]
    fn mapped_unparse_inlines_object_zero_as_null() {
        let array = ObjectHandle::array(vec![ObjectHandle::new_indirect_unresolved(
            ObjectRef::new(0, 0),
            0,
        )]);
        let map = |_object_ref| -> crate::Result<ObjectRef> {
            panic!("object zero map call") // cov:ignore: intentional panic guard
        };

        let mut array_out = Vec::new();
        array
            .unparse_object_with_ref_map(&mut array_out, &map)
            .unwrap();
        assert_eq!(array_out, b"[ null ]");

        let reference = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        reference.set_resolved(ObjectValue::Reference(ObjectRef::new(0, 0)));
        let mut reference_out = Vec::new();
        reference
            .unparse_object_with_ref_map(&mut reference_out, &map)
            .unwrap();
        assert_eq!(reference_out, b"null");
    }

    #[test]
    fn unparse_stream_body_suppresses_a_null_valued_key() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Length".to_vec(), ObjectHandle::integer(3)),
            (b"Metadata".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(out, b"<< /Length 3 >>");
    }

    #[test]
    fn unparse_stream_body_uses_the_dictionary_of_a_direct_stream_value() {
        // Mirrors unparse_object_inlines_only_the_dictionary_of_a_direct_stream_value
        // above: a *direct* Stream ObjectValue has no qpdf counterpart (a
        // real QPDFObjectHandle's resolved value is never itself a stream
        // outside an indirect object), but `unparse_stream_body` must still
        // use its `stream_dict`'s entries rather than falling into the
        // non-dictionary-self `<< >>` degrade below -- keeping the promise
        // those two `unparse_object`/`unparse_object_qdf` tests made on this
        // primitive's behalf ("`unparse_stream_body` (Task 6) is separately
        // responsible for this").
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        handle.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_stream_body_on_an_indirect_handle_resolving_to_a_stream_uses_the_dictionary() {
        // Mirrors unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary
        // above: a real, reachable qpdf shape -- an indirect object whose
        // resolved value is a stream (e.g. a production reader's own
        // resolution of a stream object). The mock-resolver harness resolves
        // `self` to `Stream { stream_dict, .. }` the same way that reader
        // would; `stream_dict`'s own entries must still surface here rather
        // than the non-dictionary-self `<< >>` degrade.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_stream_body_resolves_an_unresolved_indirect_stream_dict() {
        // `stream_dict` is itself an `ObjectHandle` that may not yet be
        // resolved (e.g. a production reader's lazily-resolved stream
        // dictionary), not just an already-direct one as the two tests
        // above build. `self` stays a *direct* Stream value here so the
        // only variable under test is `stream_dict`'s own resolution state:
        // without the `stream_dict.try_dereference()?` call this fix added,
        // `with_value` on a not-yet-resolved indirect handle returns `None`
        // and this would degrade to `<< >>` instead of using the resolved
        // dictionary's entries.
        let (inner, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [(b"Length".to_vec(), ObjectHandle::integer(2))]
                .into_iter()
                .collect(),
        ));
        let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        handle.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_stream_body_propagates_a_dropped_document_error_from_stream_dict() {
        // Mirrors unparse_stream_body_propagates_a_dropped_document_error
        // below, but for the new Stream-handling path added by this fix:
        // the dropped document lives behind `stream_dict`, not `self`
        // directly (`self` is a *direct* Stream value; only `stream_dict`
        // is the as-yet-unresolved indirect handle whose resolver is
        // dropped). The new `stream_dict.try_dereference()?` call must
        // surface this error too, not silently degrade to an empty `<< >>`
        // the way an unresolved `with_value` read alone would.
        let (inner, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        assert!(handle.unparse_stream_body(&mut out, false).is_err());
    }

    #[test]
    fn unparse_stream_body_writes_empty_dict_when_stream_dict_is_not_a_dictionary() {
        // `stream_dict` is itself typed as an `ObjectHandle`, so nothing at
        // the type level prevents it from resolving to something other than
        // a `Dictionary` -- mirroring the same typed-input assumption
        // `self` itself is held to by
        // unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self
        // below. Exercises the new nested `_ => Vec::new()` arm for
        // `stream_dict`'s own resolved value.
        let handle = ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        handle.unparse_stream_body(&mut out, false).unwrap();
        assert_eq!(out, b"<< >>");
    }

    #[test]
    fn unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self() {
        // Pins the doc comment's typed-input-assumption claim: a
        // non-dictionary `self` (mirroring `write_pdf_stream`'s own
        // assumption that it is only ever called on a stream's dictionary)
        // writes an empty `<< >>` rather than panicking or erroring.
        let mut out = Vec::new();
        ObjectHandle::integer(5)
            .unparse_stream_body(&mut out, false)
            .unwrap();
        assert_eq!(out, b"<< >>");
    }

    #[test]
    fn unparse_stream_body_propagates_a_dropped_document_error() {
        // Mirrors unparse_object_propagates_a_dropped_document_error: an
        // as-yet-unresolved indirect handle whose document has been dropped
        // must surface as an error here too, not silently degrade to an
        // empty `<< >>` the way an unresolved `with_value` read alone would.
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.unparse_stream_body(&mut out, false).is_err());
    }

    // QDF-mode sibling suite of the `unparse_stream_body_*` tests above,
    // for `unparse_stream_body_qdf`. Every hardcoded expected byte string
    // below was cross-checked against a live call to
    // `Dictionary::write_pdf_stream_qdf` (`object.rs`) with an equivalent
    // dictionary before being pinned here, not hand-derived from reading
    // the algorithm alone -- see this primitive's own doc for the full
    // qpdf-correspondence and the deliberate absence of a `refiltered`
    // parameter.

    #[test]
    fn unparse_stream_body_qdf_writes_length_last_preserved() {
        // No `refiltered` dimension exists for the QDF shape (see
        // `unparse_stream_body_qdf`'s own doc for why), so unlike its
        // compact sibling this has only one shape to pin: every other key
        // stays at its natural alphabetical position, and `/Length` is
        // pulled out and written last, immediately before the closing
        // `>>`. Deliberately includes `/Width`, which sorts *after*
        // `/Length` alphabetically (`DecodeParms` < `Filter` < `Length` <
        // `Width`): with only `{Filter, DecodeParms, Length}` (no key past
        // `Length`), `/Length`'s natural BTreeMap position already happens
        // to be last, so a broken implementation that forgot the pull-out
        // entirely would still pass -- `/Width` makes the two shapes
        // actually diverge (mutation-tested: deleting the pull-out
        // `key.as_slice() == b"Length"` branch does not fail this suite
        // without `/Width` present). Cross-checked against
        // `Dictionary::write_pdf_stream_qdf` with the equivalent dict:
        // `<<\n  /DecodeParms <<\n  >>\n  /Filter /FlateDecode\n  /Width 100\n  /Length 3\n>>`.
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
            (b"Width".to_vec(), ObjectHandle::integer(100)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(
            out,
            b"<<\n  /DecodeParms <<\n  >>\n  /Filter /FlateDecode\n  /Width 100\n  /Length 3\n>>"
        );
    }

    #[test]
    fn unparse_stream_body_qdf_writes_sig_contents_as_a_hex_string() {
        // QDF sibling of `unparse_stream_body_writes_sig_contents_as_a_hex_string`
        // above -- `unparse_stream_dict_entries_qdf` applies the same
        // Sig+ByteRange special case its compact sibling does (see that
        // function's own doc). `/Length` is pulled out and written last, at
        // `indent + 2`, same as the non-Sig QDF stream shape above.
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
            (b"Length".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(
            out,
            b"<<\n  /ByteRange [\n  ]\n  /Contents <6869>\n  /Type /Sig\n  /Length 2\n>>"
        );
    }

    #[test]
    fn unparse_stream_body_qdf_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached(
    ) {
        // QDF-stream sibling of
        // `unparse_object_defers_type_and_byte_range_resolution_until_a_contents_key_is_reached`
        // above -- `unparse_stream_dict_entries_qdf` needed the identical
        // fix at its own call site. See that test's own doc for the full
        // eager-vs-lazy rationale and why a plain success-vs-error
        // assertion cannot discriminate this bug.
        let (aaa, aaa_resolver) = error_resolving_handle(ObjectRef::new(99, 0));
        drop(aaa_resolver);
        let (sig_type, type_resolver) = error_resolving_handle(ObjectRef::new(30, 0));
        drop(type_resolver);
        let dict = ObjectHandle::dictionary(vec![
            (b"AAA".to_vec(), aaa),
            (b"Type".to_vec(), sig_type),
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Length".to_vec(), ObjectHandle::integer(0)),
        ]);
        let mut out = Vec::new();
        let error = dict.unparse_stream_body_qdf(&mut out, 0).unwrap_err();
        assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
    }

    #[test]
    fn unparse_stream_body_qdf_suppresses_a_null_valued_key() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Length".to_vec(), ObjectHandle::integer(3)),
            (b"Metadata".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
        // Cross-checked against a direct `Dictionary::write_pdf_stream_qdf`
        // call on the equivalent dict *without* the null key removed: that
        // call writes `/Metadata null` verbatim (`write_pdf_stream_qdf`
        // itself applies no null suppression -- that is layered on top by
        // `visible_dict_entries`, exactly like the compact
        // `unparse_stream_dict_entries` does), confirming the suppression
        // observed here is this primitive's own added behavior, not
        // something already built into the legacy function it delegates
        // scalar/container formatting to.
        assert_eq!(out, b"<<\n  /Length 3\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_writes_an_empty_dict_when_every_entry_is_suppressed() {
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::null())]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
        // No surviving entries and no `/Length`: matches
        // `write_pdf_stream_qdf`'s own empty-input shape `<<\n>>` (no
        // interior spaces at indent 0 -- `push_spaces(indent)` only adds
        // spaces when `indent > 0`).
        assert_eq!(out, b"<<\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_respects_a_nonzero_indent() {
        // Mirrors unparse_object_qdf_respects_a_nonzero_starting_indent:
        // every other QDF test in this suite pins `indent = 0`, which would
        // still pass with a stray hardcoded `0` inside the function body.
        // Both values here are scalars, so this alone does not prove the
        // `indent + 2` passed to `write_child_qdf` for each entry actually
        // carries the caller's `indent` -- a scalar ignores that argument
        // entirely (see `unparse_stream_body_qdf_respects_a_nonzero_indent_for_a_nested_container_value`
        // below for the test that does). Cross-checked against
        // `Dictionary::write_pdf_stream_qdf(&mut out, 4)` on the equivalent
        // dict: `<<\n      /A 1\n      /Length 2\n    >>`.
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"Length".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 4).unwrap();
        assert_eq!(out, b"<<\n      /A 1\n      /Length 2\n    >>");
    }

    #[test]
    fn unparse_stream_body_qdf_respects_a_nonzero_indent_for_a_nested_container_value() {
        // The test above only proves `indent` reaches the closing `>>`'s
        // own `push_spaces` and the entry lines' *leading* `push_spaces(out,
        // indent + 2)` -- both scalar values in it ignore the `indent + 2`
        // this primitive also threads through `write_child_qdf(value,
        // indent + 2, out)` for each entry (a scalar's own QDF form does
        // not depend on indent at all). A nested container value does: its
        // own children land at `(indent + 2) + 2`, and a hardcoded `2` in
        // place of that `indent + 2` argument would still pass every other
        // test in this suite (mutation-tested: it survives without this
        // test). Cross-checked against `Dictionary::write_pdf_stream_qdf`
        // with the equivalent dict at indent 4:
        // `<<\n      /DecodeParms <<\n        /Predictor 12\n      >>\n      /Length 3\n    >>`.
        let dict = ObjectHandle::dictionary(vec![
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), ObjectHandle::integer(12))]),
            ),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 4).unwrap();
        assert_eq!(
            out,
            b"<<\n      /DecodeParms <<\n        /Predictor 12\n      >>\n      /Length 3\n    >>"
        );
    }

    #[test]
    fn unparse_stream_body_qdf_writes_a_retained_indirect_entry_as_reference_form() {
        // QDF-mode sibling of unparse_stream_body_writes_length_last_preserved
        // that exercises write_child_qdf's indirect arm instead of a direct
        // scalar -- unreached by the tests above, whose every value is
        // direct.
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), indirect),
            (b"Length".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut out = Vec::new();
        dict.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /A 20 0 R\n  /Length 2\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_uses_the_dictionary_of_a_direct_stream_value() {
        // Mirrors unparse_stream_body_uses_the_dictionary_of_a_direct_stream_value:
        // a *direct* Stream ObjectValue has no qpdf counterpart, but this
        // primitive must still use its `stream_dict`'s entries rather than
        // falling into the non-dictionary-self `<< >>` degrade below.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        handle.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /Length 2\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_on_an_indirect_handle_resolving_to_a_stream_uses_the_dictionary() {
        // Mirrors unparse_stream_body_on_an_indirect_handle_resolving_to_a_stream_uses_the_dictionary:
        // a real, reachable qpdf shape -- an indirect object whose resolved
        // value is a stream (e.g. a production reader's own resolution of a
        // stream object).
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /Length 2\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_resolves_an_unresolved_indirect_stream_dict() {
        // Mirrors unparse_stream_body_resolves_an_unresolved_indirect_stream_dict:
        // `stream_dict` is itself an `ObjectHandle` that may not yet be
        // resolved. `self` stays a *direct* Stream value here so the only
        // variable under test is `stream_dict`'s own resolution state:
        // without the `stream_dict.try_dereference()?` call this primitive
        // makes, `with_value` on a not-yet-resolved indirect handle returns
        // `None` and this would degrade to `<< >>` instead of using the
        // resolved dictionary's entries.
        let (inner, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [(b"Length".to_vec(), ObjectHandle::integer(2))]
                .into_iter()
                .collect(),
        ));
        let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        handle.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /Length 2\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_propagates_a_dropped_document_error_from_stream_dict() {
        // Mirrors unparse_stream_body_propagates_a_dropped_document_error_from_stream_dict:
        // the dropped document lives behind `stream_dict`, not `self`
        // directly (`self` is a *direct* Stream value; only `stream_dict`
        // is the as-yet-unresolved indirect handle whose resolver is
        // dropped).
        let (inner, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let handle = ObjectHandle::stream(inner, Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        assert!(handle.unparse_stream_body_qdf(&mut out, 0).is_err());
    }

    #[test]
    fn unparse_stream_body_qdf_writes_empty_dict_when_stream_dict_is_not_a_dictionary() {
        // Mirrors unparse_stream_body_writes_empty_dict_when_stream_dict_is_not_a_dictionary:
        // `stream_dict` is itself typed as an `ObjectHandle`, so nothing at
        // the type level prevents it from resolving to something other than
        // a `Dictionary`.
        let handle = ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        handle.unparse_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_writes_empty_dict_for_a_non_dictionary_self() {
        // Mirrors unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self:
        // pins the doc comment's typed-input-assumption claim.
        let mut out = Vec::new();
        ObjectHandle::integer(5)
            .unparse_stream_body_qdf(&mut out, 0)
            .unwrap();
        assert_eq!(out, b"<<\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_propagates_a_dropped_document_error() {
        // Mirrors unparse_stream_body_propagates_a_dropped_document_error:
        // an as-yet-unresolved indirect handle whose document has been
        // dropped must surface as an error here too, not silently degrade
        // to an empty `<< >>` the way an unresolved `with_value` read alone
        // would.
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.unparse_stream_body_qdf(&mut out, 0).is_err());
    }

    #[test]
    fn unparse_trailer_classic_forces_id_and_encrypt_last() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Size".to_vec(), ObjectHandle::integer(9)),
            (b"Root".to_vec(), ObjectHandle::integer(1)), // stand-in reference shape
            (b"Encrypt".to_vec(), ObjectHandle::integer(9)),
            (
                b"ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(vec![0u8; 16]),
                    ObjectHandle::string(vec![1u8; 16]),
                ]),
            ),
        ]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("trailer << "));
        assert!(text.ends_with(">>"));
        // /Root and /Size appear before /ID, /ID appears before /Encrypt,
        // regardless of the dict's own (alphabetical) key order.
        let root_pos = text.find("/Root").unwrap();
        let id_pos = text.find("/ID").unwrap();
        let encrypt_pos = text.find("/Encrypt").unwrap();
        assert!(root_pos < id_pos);
        assert!(id_pos < encrypt_pos);
    }

    #[test]
    fn unparse_trailer_xref_stream_does_not_write_its_own_open_brace() {
        // The caller (a future `writeXRefStream`-shaped consumer) has
        // already opened `<<` and hand-emitted the xref-specific keys
        // before calling this method with `xref_stream = true` -- see
        // this method's own doc for why those keys are never part of
        // `entries` here to begin with.
        let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, true, None).unwrap();
        assert!(!String::from_utf8_lossy(&out).contains("<<"));
        assert!(String::from_utf8_lossy(&out).ends_with(">>"));
    }

    #[test]
    fn unparse_trailer_without_id_or_encrypt_omits_both() {
        let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("/ID"));
        assert!(!text.contains("/Encrypt"));
    }

    #[test]
    fn unparse_trailer_does_not_suppress_a_null_valued_key() {
        // writeTrailer's own key loop has no isNull check anywhere in it
        // (QPDFWriter.cc:1174-1192) -- unlike unparseObject's dictionary
        // branch. `/Prev` is chosen only as a convenient never-suppressed
        // example key for this unit-level test; in production `/Prev`
        // itself would already have been stripped by the caller before
        // this primitive ever sees the dict (see this method's own doc on
        // the `getTrimmedTrailer`-equivalent split), so this does not
        // claim `/Prev` null survives end to end -- only that *this*
        // primitive's key loop applies no suppression to whatever keys
        // the caller does hand it.
        let dict = ObjectHandle::dictionary(vec![
            (b"Size".to_vec(), ObjectHandle::integer(9)),
            (b"Prev".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        assert!(String::from_utf8_lossy(&out).contains("/Prev null"));
    }

    #[test]
    fn unparse_trailer_id_writer_substitutes_the_id_value() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Size".to_vec(), ObjectHandle::integer(9)),
            (
                b"ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(vec![0u8; 16]),
                    ObjectHandle::string(vec![0u8; 16]),
                ]),
            ),
        ]);
        let mut out = Vec::new();
        let mut id_writer = |out: &mut Vec<u8>| out.extend_from_slice(b"<computed>");
        dict.unparse_trailer(&mut out, false, Some(&mut id_writer))
            .unwrap();
        assert!(String::from_utf8_lossy(&out).contains("/ID <computed>"));
    }

    #[test]
    fn unparse_trailer_id_writer_none_uses_qpdf_compact_hex_shape() {
        // Pins the exact byte shape unparse_trailer_id_writer_substitutes_the_id_value's
        // dict (all-zero bytes) can't distinguish from a generic array
        // serialization -- mirrors write_id_style_value_emits_compact_hex_pair
        // (object.rs) byte-for-byte.
        let dict = ObjectHandle::dictionary(vec![
            (b"Size".to_vec(), ObjectHandle::integer(9)),
            (
                b"ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(vec![0xabu8, 0xcdu8, 0xefu8]),
                    ObjectHandle::string(vec![0x12u8, 0x34u8, 0x56u8]),
                ]),
            ),
        ]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        assert_eq!(out, b"trailer << /Size 9 /ID [<abcdef><123456>] >>");
    }

    #[test]
    fn unparse_trailer_id_writer_none_falls_back_for_unexpected_id_shape() {
        // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
        // (object.rs): wrong arity falls back to the generic array
        // serializer (spaces) rather than silently truncating to two
        // elements.
        let dict = ObjectHandle::dictionary(vec![(
            b"ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(vec![0x00u8]),
                ObjectHandle::string(vec![0x11u8]),
                ObjectHandle::string(vec![0x8fu8]),
            ]),
        )]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        assert_eq!(out, b"trailer << /ID [ <00> <11> <8f> ] >>");
    }

    #[test]
    fn unparse_trailer_id_writer_none_falls_back_for_non_string_element() {
        // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
        // (object.rs): right arity (2 elements) but a non-String element
        // type falls back to the generic array serializer rather than
        // treating the element as a string.
        let dict = ObjectHandle::dictionary(vec![(
            b"ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(1),
                ObjectHandle::string(vec![0x8fu8]),
            ]),
        )]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        assert_eq!(out, b"trailer << /ID [ 1 <8f> ] >>");
    }

    #[test]
    fn unparse_trailer_id_writer_none_falls_back_for_scalar_id() {
        // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
        // (object.rs): a non-array /ID value is delegated to write_child
        // verbatim rather than being routed through the compact-pair path.
        let dict = ObjectHandle::dictionary(vec![(b"ID".to_vec(), ObjectHandle::integer(7))]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        assert_eq!(out, b"trailer << /ID 7 >>");
    }

    #[test]
    fn unparse_trailer_writes_an_indirect_id_as_reference_form_not_inlined() {
        // `object_ref().is_some()` must be checked before shape inspection
        // in write_id_style_value_handle: an indirect /ID value (not a
        // shape real qpdf itself ever produces, but nothing at the type
        // level rules it out) writes as its own "N G R" form, the same
        // reference-vs-recurse split write_child applies everywhere else
        // in this primitive family -- never inlined as compact hex even
        // though it would resolve to a matching Array([String, String])
        // shape.
        let (indirect_id, _resolver) = resolver_bearing_handle(ObjectValue::Array(vec![
            ObjectHandle::string(vec![0u8; 2]),
            ObjectHandle::string(vec![1u8; 2]),
        ]));
        let dict = ObjectHandle::dictionary(vec![
            (b"Size".to_vec(), ObjectHandle::integer(9)),
            (b"ID".to_vec(), indirect_id),
        ]);
        let mut out = Vec::new();
        dict.unparse_trailer(&mut out, false, None).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("/ID 20 0 R"));
        assert!(!text.contains("[<"));
    }

    #[test]
    fn unparse_trailer_writes_empty_shell_for_a_non_dictionary_self() {
        // Mirrors unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self:
        // a non-dictionary `self` degrades to an empty trailer shell
        // rather than panicking or erroring, matching write_pdf_trailer's
        // own typed-input assumption.
        let mut out = Vec::new();
        ObjectHandle::integer(5)
            .unparse_trailer(&mut out, false, None)
            .unwrap();
        assert_eq!(out, b"trailer << >>");
    }

    #[test]
    fn unparse_trailer_resolves_an_unresolved_indirect_trailer_dict() {
        // Without the `self.try_dereference()?` call this method makes
        // before `with_value`, `with_value` on a not-yet-resolved
        // indirect handle returns `None` and this would degrade to an
        // empty `trailer << >>` shell instead of using the resolved
        // dictionary's entries -- mirrors
        // unparse_stream_body_resolves_an_unresolved_indirect_stream_dict's
        // same proof for the same fix pattern.
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Dictionary(
            [(b"Size".to_vec(), ObjectHandle::integer(9))]
                .into_iter()
                .collect(),
        ));
        let mut out = Vec::new();
        indirect.unparse_trailer(&mut out, false, None).unwrap();
        assert_eq!(out, b"trailer << /Size 9 >>");
    }

    #[test]
    fn unparse_trailer_propagates_a_dropped_document_error() {
        // Mirrors unparse_stream_body_propagates_a_dropped_document_error:
        // an as-yet-unresolved indirect handle whose document has been
        // dropped must surface as an error here too, not silently degrade
        // to an empty `trailer << >>` shell the way an unresolved
        // `with_value` read alone would.
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.unparse_trailer(&mut out, false, None).is_err());
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::*;

    struct SourcePipeResolver {
        value: ObjectValue,
        bytes: Vec<u8>,
        calls: RefCell<Vec<(bool, bool)>>,
        warnings: RefCell<Vec<String>>,
        fail_first: bool,
        fail_with_error: bool,
    }

    impl DocumentResolver for SourcePipeResolver {
        fn warn(&self, message: String) -> crate::Result<()> {
            self.warnings.borrow_mut().push(message);
            Ok(())
        }

        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            handle.set_resolved(self.value.clone());
            Ok(())
        }

        fn pipe_stream_data(
            &self,
            _object_ref: ObjectRef,
            _offset: i64,
            _length: usize,
            _stream_dict: &ObjectHandle,
            pipeline: &mut dyn Pipeline,
            suppress_warnings: bool,
            will_retry: bool,
        ) -> crate::Result<bool> {
            self.calls
                .borrow_mut()
                .push((suppress_warnings, will_retry));
            if self.fail_with_error {
                return Err(crate::Error::Internal("source pipe failure".to_owned()));
            }
            if self.fail_first && self.calls.borrow().len() == 1 {
                return Ok(false);
            }
            pipeline.write(&self.bytes)?;
            pipeline.finish()?;
            Ok(true)
        }
    }

    #[test]
    fn pipe_stream_data_rejects_a_non_stream_handle() {
        let scalar = ObjectHandle::integer(7);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        let error = scalar
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::None,
                false,
                false,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::Internal(message) if message == "pipeStreamData called for non-stream"
        ));
    }

    #[test]
    fn pipe_stream_data_decodes_replaced_flate_through_the_sink() {
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let stream = ObjectHandle::stream(
            dict,
            Rc::new(vec![
                0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
            ]),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        let success = stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .expect("replaced stream should decode");

        assert!(success);
        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"hello");
    }

    #[test]
    fn stream_pipeline_error_mapping_keeps_non_codec_errors_fatal() {
        assert!(matches!(
            ObjectHandle::map_stream_pipeline_error(PipelineError::logic("sink failed"), true),
            Error::Internal(message) if message == "sink failed"
        ));
        assert!(matches!(
            ObjectHandle::map_stream_pipeline_error(PipelineError::runtime("sink failed"), false),
            Error::System(message) if message == "sink failed"
        ));
    }

    #[test]
    fn pipe_stream_data_builds_reverse_decoder_chain() {
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ]),
            ),
            (b"DecodeParms".to_vec(), ObjectHandle::array(vec![])),
        ]);
        let stream = ObjectHandle::stream(dict, Rc::new(b"789ccb48cdc9c90700062c0215>".to_vec()));
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"hello");
    }

    #[test]
    fn pipe_stream_data_reads_original_source_through_the_filter_chain() {
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let encoded = vec![
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
        ];
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: dict,
                stream_data: None,
                stream_provider: None,
                stream_length: encoded.len(),
            },
            bytes: encoded,
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"hello");
        assert_eq!(resolver.calls.borrow().as_slice(), &[(false, false)]);
    }

    #[test]
    fn pipe_stream_data_propagates_a_filtered_source_error() {
        let raw = b"source error".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                )]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw,
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: true,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        let error = stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap_err();

        assert!(matches!(
            error,
            Error::Internal(message) if message == "source pipe failure"
        ));
        assert!(filtering_attempted);
        assert_eq!(resolver.calls.borrow().as_slice(), &[(false, false)]);
    }

    #[test]
    fn get_stream_data_maps_a_filtered_source_retry_to_an_error() {
        let raw = b"source error".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                )]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw,
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: true,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);

        let error = stream
            .get_stream_data(crate::writer::DecodeLevel::Generalized)
            .expect_err("a failed filtered source must not be reported as decoded data");

        assert!(matches!(
            error,
            Error::Unsupported(message) if message == "error getting decoded stream data"
        ));
        assert_eq!(resolver.calls.borrow().as_slice(), &[(false, false)]);
    }

    #[test]
    fn pipe_stream_data_resolves_filter_and_decode_parameter_values_through_the_document() {
        let (filter, _filter_resolver) = super::identity_tests::resolver_bearing_handle(
            ObjectValue::Name(b"FlateDecode".to_vec()),
        );
        let (predictor, _predictor_resolver) =
            super::identity_tests::resolver_bearing_handle(ObjectValue::Integer(1));
        let dict = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), filter),
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), predictor)]),
            ),
        ]);
        let stream = ObjectHandle::stream(
            dict,
            Rc::new(vec![
                0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
            ]),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"hello");
    }

    #[test]
    fn pipe_stream_data_reports_filter_failure_separately_for_raw_retry() {
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let raw = vec![1, 2, 3, 4];
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: dict,
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw.clone(),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: true,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);

        let mut failed_sink = crate::pipeline::buffer::Buffer::new("failed", None);
        let mut filtering_attempted = false;
        assert!(!stream
            .pipe_stream_data(
                &mut failed_sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());
        // qpdf clears its filtering flag when the original source reports a
        // failure, even though the filter chain was constructed. The caller
        // uses the false result and flag to enter the raw retry path.
        assert!(!filtering_attempted);

        let mut retry_sink = crate::pipeline::buffer::Buffer::new("retry", None);
        assert!(stream
            .pipe_stream_data(
                &mut retry_sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::None,
                false,
                true,
            )
            .unwrap());
        assert!(!filtering_attempted);
        assert_eq!(retry_sink.take_buffer().unwrap(), raw);
        assert_eq!(
            resolver.calls.borrow().as_slice(),
            &[(false, false), (false, true)]
        );
    }

    #[test]
    fn pipe_stream_data_preserves_an_unsupported_filter_without_attempting_filtering() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"Filter".to_vec(),
                ObjectHandle::name(b"UnknownDecode".to_vec()),
            )]),
            Rc::new(b"raw".to_vec()),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"raw");
    }

    #[test]
    fn pipe_stream_data_checks_unknown_factory_before_decode_parms_shape() {
        let raw = b"raw unknown".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![
                    (
                        b"Filter".to_vec(),
                        ObjectHandle::name(b"UnknownDecode".to_vec()),
                    ),
                    (
                        b"DecodeParms".to_vec(),
                        ObjectHandle::array(vec![ObjectHandle::null(), ObjectHandle::null()]),
                    ),
                ]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw.clone(),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), raw);
        assert!(resolver.warnings.borrow().is_empty());
    }

    #[test]
    fn pipe_stream_data_warns_for_mismatched_decode_parms_before_source() {
        let raw = b"raw mismatch".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![
                    (
                        b"Filter".to_vec(),
                        ObjectHandle::array(vec![
                            ObjectHandle::name(b"FlateDecode".to_vec()),
                            ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                        ]),
                    ),
                    (
                        b"DecodeParms".to_vec(),
                        ObjectHandle::array(vec![ObjectHandle::null()]),
                    ),
                ]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw.clone(),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), raw);
        assert_eq!(
            resolver.warnings.borrow().as_slice(),
            &["object 20 0: stream /DecodeParms length is inconsistent with filters"]
        );
    }

    #[test]
    fn pipe_stream_data_applies_aligned_decode_parms_across_filter_stages() {
        let inner = crate::stream_filter::encode_flate(b"hello").unwrap();
        let encoded = crate::stream_filter::encode_flate(&inner).unwrap();
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ]),
            ),
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::dictionary(vec![(
                        b"Predictor".to_vec(),
                        ObjectHandle::integer(1),
                    )]),
                    ObjectHandle::dictionary(vec![(
                        b"Predictor".to_vec(),
                        ObjectHandle::integer(1),
                    )]),
                ]),
            ),
        ]);
        let stream = ObjectHandle::stream(dict, Rc::new(encoded));
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"hello");
    }

    #[test]
    fn pipe_stream_data_keeps_crypt_as_a_no_stage_after_filterability() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec())),
                (
                    b"DecodeParms".to_vec(),
                    ObjectHandle::dictionary(vec![(
                        b"Name".to_vec(),
                        ObjectHandle::name(b"Identity".to_vec()),
                    )]),
                ),
            ]),
            Rc::new(b"already clear".to_vec()),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"already clear");
    }

    #[test]
    fn pipe_stream_data_reports_flate_warnings_through_the_object_sink() {
        let raw = vec![0x78];
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                )]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw,
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert!(sink.take_buffer().unwrap().is_empty());
        assert_eq!(
            resolver.warnings.borrow().as_slice(),
            &["object 20 0: input stream is complete but output may still be valid"]
        );
    }

    #[test]
    fn pipe_stream_data_reports_normalizer_warnings_only_after_source_success() {
        let raw = b"<0g".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw.clone(),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                STREAM_ENCODE_NORMALIZE,
                crate::writer::DecodeLevel::None,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), raw);
        assert_eq!(
            resolver.warnings.borrow().as_slice(),
            &[
                "object 20 0: content normalization encountered bad tokens",
                "object 20 0: normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents",
                "object 20 0: Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.",
            ]
        );

        resolver.warnings.borrow_mut().clear();
        let mut suppressed_sink = crate::pipeline::buffer::Buffer::new("suppressed", None);
        assert!(stream
            .pipe_stream_data(
                &mut suppressed_sink,
                &mut filtering_attempted,
                STREAM_ENCODE_NORMALIZE,
                crate::writer::DecodeLevel::None,
                true,
                false,
            )
            .unwrap());
        assert!(filtering_attempted);
        assert_eq!(suppressed_sink.take_buffer().unwrap(), raw);
        assert!(resolver.warnings.borrow().is_empty());
    }

    #[test]
    fn pipe_stream_data_gates_specialized_filters_by_decode_level() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"Filter".to_vec(),
                ObjectHandle::name(b"DCTDecode".to_vec()),
            )]),
            Rc::new(b"jpeg bytes".to_vec()),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"jpeg bytes");
    }

    #[test]
    fn pipe_stream_data_gates_non_lossy_specialized_filters_separately() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"Filter".to_vec(),
                ObjectHandle::name(b"RunLengthDecode".to_vec()),
            )]),
            Rc::new(vec![0xff, b'A', 0x80]),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), vec![0xff, b'A', 0x80]);
    }

    #[test]
    fn pipe_stream_data_accepts_a_qpdf_unbounded_filter_chain() {
        let filter_count = 17;
        let mut filters = Vec::with_capacity(filter_count);
        let mut encoded = b"A".to_vec();
        for _ in 0..filter_count {
            let mut wrapped = Vec::with_capacity(encoded.len() * 2 + 1);
            for byte in encoded {
                wrapped.extend_from_slice(format!("{byte:02x}").as_bytes());
            }
            wrapped.push(b'>');
            encoded = wrapped;
            filters.push(ObjectHandle::name(b"ASCIIHexDecode".to_vec()));
        }
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"Filter".to_vec(), ObjectHandle::array(filters))]),
            Rc::new(encoded),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"A");
    }

    #[test]
    fn pipe_stream_data_warns_and_retries_raw_for_a_malformed_filter_shape() {
        let raw = b"raw malformed".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::integer(7),
                )]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw.clone(),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), raw);
        assert_eq!(
            resolver.warnings.borrow().as_slice(),
            &["object 20 0: stream filter type is not name or array"]
        );
    }

    #[test]
    fn pipe_stream_data_warns_for_a_non_name_filter_array_item() {
        let raw = b"raw malformed array".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::name(b"FlateDecode".to_vec()),
                        ObjectHandle::integer(7),
                    ]),
                )]),
                stream_data: None,
                stream_provider: None,
                stream_length: raw.len(),
            },
            bytes: raw.clone(),
            calls: RefCell::new(Vec::new()),
            warnings: RefCell::new(Vec::new()),
            fail_first: false,
            fail_with_error: false,
        });
        let resolver_handle: Rc<dyn DocumentResolver> = resolver.clone();
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        stream.set_parsed_offset_if_unset(9);
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), raw);
        assert_eq!(
            resolver.warnings.borrow().as_slice(),
            &["object 20 0: stream filter type is not name or array"]
        );
    }

    #[test]
    fn pipe_stream_data_rejects_present_decode_parms_for_dct_before_stage_build() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"DCTDecode".to_vec()),
                ),
                (
                    b"DecodeParms".to_vec(),
                    ObjectHandle::dictionary(vec![(
                        b"Predictor".to_vec(),
                        ObjectHandle::integer(1),
                    )]),
                ),
            ]),
            Rc::new(b"raw jpeg".to_vec()),
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = true;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::All,
                false,
                false,
            )
            .unwrap());

        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().unwrap(), b"raw jpeg");
    }

    #[test]
    fn pipe_stream_data_normalizes_before_compressing_output() {
        let stream =
            ObjectHandle::stream(ObjectHandle::dictionary(vec![]), Rc::new(b"q\rQ".to_vec()));
        let mut sink = crate::pipeline::buffer::Buffer::new("sink", None);
        let mut filtering_attempted = false;

        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                STREAM_ENCODE_COMPRESS | STREAM_ENCODE_NORMALIZE,
                crate::writer::DecodeLevel::None,
                false,
                false,
            )
            .unwrap());

        let compressed = sink.take_buffer().unwrap();
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(compressed.as_slice()),
            &mut decoded,
        )
        .unwrap();
        assert!(filtering_attempted);
        assert_eq!(decoded, b"q\nQ");
    }

    #[test]
    fn object_value_clone_preserves_scalar_content() {
        let value = ObjectValue::Integer(42);
        let cloned = value.clone();
        assert!(matches!(cloned, ObjectValue::Integer(42)));
    }

    #[test]
    fn object_value_clone_of_a_dictionary_shares_child_identity() {
        let child = ObjectHandle::integer(7);
        let dict = ObjectValue::Dictionary([(b"/K".to_vec(), child.clone())].into_iter().collect());
        let cloned = dict.clone();
        let ObjectValue::Dictionary(entries) = cloned else {
            panic!("expected dictionary"); // cov:ignore: unreachable in a passing run
        };
        assert!(entries.get(b"/K".as_slice()).unwrap().ptr_eq(&child));
    }

    #[test]
    fn get_key_returns_a_live_child_handle_without_snapshotting_the_dictionary() {
        let child = ObjectHandle::integer(1);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), child.clone())]);
        let fetched = dict.get_key(b"/A");
        assert!(fetched.ptr_eq(&child));
    }

    #[test]
    fn get_key_on_a_missing_key_returns_a_direct_null_handle() {
        let dict = ObjectHandle::dictionary(vec![]);
        assert!(dict.get_key(b"/Missing").is_null());
    }

    #[test]
    fn get_key_on_a_non_dictionary_handle_returns_a_direct_null_handle() {
        let scalar = ObjectHandle::integer(5);
        assert!(scalar.get_key(b"/A").is_null());
    }

    #[test]
    fn replace_key_mutates_the_live_dictionary_in_place() {
        let dict = ObjectHandle::dictionary(vec![]);
        let clone = dict.clone();
        dict.replace_key(b"/A", ObjectHandle::integer(9));
        assert_eq!(clone.get_key(b"/A").as_integer(), Some(9));
    }

    #[test]
    fn replace_key_overwrites_an_existing_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        dict.replace_key(b"/A", ObjectHandle::integer(2));
        assert_eq!(dict.get_key(b"/A").as_integer(), Some(2));
    }

    #[test]
    fn replace_key_with_direct_null_removes_an_existing_key_and_detaches_its_child() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let child = ObjectHandle::dictionary(vec![]);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), child.clone())]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Nested".to_vec(), dict.clone())].into_iter().collect(),
        ));

        dict.replace_key(b"/A", ObjectHandle::null());

        assert!(!dict.has_key(b"/A"));
        assert!(child.containing_object_refs().is_empty());
    }

    #[test]
    fn replace_key_with_direct_null_keeps_a_missing_key_absent() {
        let dict = ObjectHandle::dictionary(vec![]);

        dict.replace_key(b"/Missing", ObjectHandle::null());

        assert!(!dict.has_key(b"/Missing"));
    }

    #[test]
    fn replace_key_with_direct_null_mutates_a_resolved_indirect_dictionary() {
        let dict = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);
        dict.set_resolved(ObjectValue::Dictionary(
            [(b"A".to_vec(), ObjectHandle::integer(1))]
                .into_iter()
                .collect(),
        ));

        dict.replace_key(b"/A", ObjectHandle::null());

        assert!(!dict.has_key(b"/A"));
    }

    #[test]
    fn replace_key_with_direct_null_on_a_non_dictionary_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);

        scalar.replace_key(b"/A", ObjectHandle::null());

        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn replace_key_preserves_a_resolved_indirect_null_and_its_identity() {
        let indirect_null = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        indirect_null.set_resolved(ObjectValue::Null);
        let dict = ObjectHandle::dictionary(vec![]);

        dict.replace_key(b"/Null", indirect_null.clone());

        let retained = dict.get_key(b"/Null");
        assert!(dict.has_key(b"/Null"));
        assert!(retained.is_indirect());
        assert!(retained.is_null());
        assert!(retained.is_same_object_as(&indirect_null));
    }

    #[test]
    fn replace_key_preserves_a_dangling_indirect_reference_and_its_identity() {
        let dangling = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        dangling.set_missing();
        let dict = ObjectHandle::dictionary(vec![]);

        dict.replace_key(b"/Dangling", dangling.clone());

        let retained = dict.get_key(b"/Dangling");
        assert!(dict.has_key(b"/Dangling"));
        assert!(retained.is_indirect());
        assert!(retained.is_null());
        assert!(retained.is_same_object_as(&dangling));
    }

    #[test]
    fn replace_key_on_a_non_dictionary_handle_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);
        scalar.replace_key(b"/A", ObjectHandle::integer(2));
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn replace_array_item_preserves_identity_and_rejects_invalid_slots() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let replacement = ObjectHandle::dictionary(vec![]);
        let retained = replacement.clone();

        assert!(array.replace_array_item(0, replacement));
        retained.replace_key(b"/K", ObjectHandle::integer(9));
        let inserted = array.as_array().expect("array")[0].clone();
        assert_eq!(inserted.get_key(b"/K").as_integer(), Some(9));

        assert!(!array.replace_array_item(1, ObjectHandle::integer(2)));
        assert!(!ObjectHandle::integer(1).replace_array_item(0, ObjectHandle::integer(2)));
        assert!(!array.replace_array_item(0, array.clone()));
    }

    #[test]
    fn canonical_array_mutators_preserve_live_aliases_and_qpdf_return_handles() {
        let first = ObjectHandle::integer(1);
        let second = ObjectHandle::integer(2);
        let array = ObjectHandle::array(vec![first.clone(), second.clone()]);
        let replacement = ObjectHandle::dictionary(vec![]);

        array
            .set_array_item(1, replacement.clone())
            .expect("setArrayItem accepts an in-bounds item");
        assert!(array
            .try_array_item(1)
            .unwrap()
            .unwrap()
            .is_same_object_as(&replacement));

        let inserted_at_front = ObjectHandle::name(b"front".to_vec());
        array
            .insert_array_item(0, inserted_at_front.clone())
            .expect("insertItem accepts zero");
        assert!(array
            .try_array_item(0)
            .unwrap()
            .unwrap()
            .is_same_object_as(&inserted_at_front));
        assert!(array
            .try_array_item(1)
            .unwrap()
            .unwrap()
            .is_same_object_as(&first));

        let inserted_middle = ObjectHandle::name(b"middle".to_vec());
        let returned = array
            .insert_array_item_and_get_new(2, inserted_middle.clone())
            .expect("insertItemAndGetNew accepts an in-range position");
        assert!(returned.is_same_object_as(&inserted_middle));
        assert!(array
            .try_array_item(2)
            .unwrap()
            .unwrap()
            .is_same_object_as(&inserted_middle));

        let appended = ObjectHandle::name(b"append".to_vec());
        array
            .append_array_item(appended.clone())
            .expect("appendItem accepts an item");
        assert!(array
            .try_array_item(4)
            .unwrap()
            .unwrap()
            .is_same_object_as(&appended));

        let appended_and_returned = ObjectHandle::name(b"append-and-get".to_vec());
        let returned = array
            .append_array_item_and_get_new(appended_and_returned.clone())
            .expect("appendItemAndGetNew accepts an item");
        assert!(returned.is_same_object_as(&appended_and_returned));
        assert!(array
            .try_array_item(5)
            .unwrap()
            .unwrap()
            .is_same_object_as(&appended_and_returned));

        let erased = array
            .erase_array_item_and_get_old(1)
            .expect("eraseItemAndGetOld returns the live old handle");
        assert!(erased.is_same_object_as(&first));
        assert_eq!(array.try_array_len().unwrap(), Some(5));

        array
            .erase_array_item(2)
            .expect("eraseItem accepts an in-bounds position");
        assert!(array
            .try_array_item(2)
            .unwrap()
            .unwrap()
            .is_same_object_as(&appended));
    }

    #[test]
    fn public_array_mutators_report_direct_self_aliases() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);

        let set_error = array
            .set_array_item(0, array.clone())
            .expect_err("direct self replacement is rejected");
        assert!(matches!(
            set_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
        let bulk_set_error = array
            .set_array_items(vec![array.clone()])
            .expect_err("direct self replacement list is rejected");
        assert!(matches!(
            bulk_set_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
        let insert_error = array
            .insert_array_item(0, array.clone())
            .expect_err("direct self insertion is rejected");
        assert!(matches!(
            insert_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
        let append_error = array
            .append_array_item(array.clone())
            .expect_err("direct self append is rejected");
        assert!(matches!(
            append_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));

        let set_error = array
            .set_array_item(usize::MAX, array.clone())
            .expect_err("bounds warning must run before the self-alias guard");
        assert!(matches!(
            set_error,
            Error::System(message)
                if message == "ignoring attempt to set out of bounds array item"
        ));
        let insert_error = array
            .insert_array_item(usize::MAX, array.clone())
            .expect_err("bounds warning must run before the self-alias guard");
        assert!(matches!(
            insert_error,
            Error::System(message)
                if message == "ignoring attempt to insert out of bounds array item"
        ));

        assert_eq!(array.try_array_len().unwrap(), Some(1));
        assert_eq!(
            array.try_array_item(0).unwrap().unwrap().as_integer(),
            Some(1)
        );
    }

    #[test]
    fn public_array_mutators_report_multi_hop_direct_cycles() {
        let first = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let second = ObjectHandle::array(vec![ObjectHandle::integer(2)]);
        first
            .set_array_item(0, second.clone())
            .expect("first direct array accepts second");
        let set_error = second
            .set_array_item(0, first.clone())
            .expect_err("reciprocal set is rejected");
        assert!(matches!(
            set_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
        assert_eq!(
            second.try_array_item(0).unwrap().unwrap().as_integer(),
            Some(2)
        );

        let first = ObjectHandle::array(vec![]);
        let second = ObjectHandle::array(vec![ObjectHandle::integer(3)]);
        first
            .insert_array_item(0, second.clone())
            .expect("first direct array accepts second");
        let insert_error = second
            .insert_array_item(0, first.clone())
            .expect_err("reciprocal insert is rejected");
        assert!(matches!(
            insert_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
        assert_eq!(second.try_array_len().unwrap(), Some(1));
        assert_eq!(
            second.try_array_item(0).unwrap().unwrap().as_integer(),
            Some(3)
        );

        let first = ObjectHandle::array(vec![]);
        let second = ObjectHandle::array(vec![ObjectHandle::integer(4)]);
        first
            .append_array_item(second.clone())
            .expect("first direct array accepts second");
        let append_error = second
            .append_array_item(first.clone())
            .expect_err("reciprocal append is rejected");
        assert!(matches!(
            append_error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
        assert_eq!(second.try_array_len().unwrap(), Some(1));
        assert_eq!(
            second.try_array_item(0).unwrap().unwrap().as_integer(),
            Some(4)
        );

        let repeated = ObjectHandle::integer(5);
        let candidate = ObjectHandle::array(vec![repeated.clone(), repeated]);
        let target = ObjectHandle::array(vec![]);
        target
            .append_array_item(candidate)
            .expect("repeated direct children do not form a cycle");
    }

    #[test]
    fn public_array_mutators_report_rejected_direct_cycles() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let error = array
            .set_array_item(0, array.clone())
            .expect_err("self set must report the rejected mutation");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));

        let array = ObjectHandle::array(vec![ObjectHandle::integer(2)]);
        let error = array
            .set_array_items(vec![array.clone()])
            .expect_err("self bulk-set must report the rejected mutation");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));

        let array = ObjectHandle::array(vec![ObjectHandle::integer(3)]);
        let error = array
            .insert_array_item(0, array.clone())
            .expect_err("self insert must report the rejected mutation");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));

        let array = ObjectHandle::array(vec![ObjectHandle::integer(4)]);
        let error = array
            .insert_array_item_and_get_new(0, array.clone())
            .expect_err("self insert-and-get must report the rejected mutation");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));

        let array = ObjectHandle::array(vec![ObjectHandle::integer(5)]);
        let error = array
            .append_array_item(array.clone())
            .expect_err("self append must report the rejected mutation");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));

        let array = ObjectHandle::array(vec![ObjectHandle::integer(6)]);
        let error = array
            .append_array_item_and_get_new(array.clone())
            .expect_err("self append-and-get must report the rejected mutation");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "attempted to create a direct object cycle"
        ));
    }

    #[test]
    fn loaded_clean_array_mutation_requires_explicit_dirty_mark() {
        let mut pdf = crate::Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec(),
        )
        .expect("open minimal PDF");
        let pages_ref = ObjectRef::new(2, 0);
        let pages = pdf.get_object_handle(pages_ref);
        pdf.resolve_object_handle(&pages)
            .expect("resolve loaded Pages object");
        let kids = pages.get_key(b"/Kids");

        assert!(pdf.dirty_object_refs().is_empty());
        kids.append_array_item(ObjectHandle::integer(4))
            .expect("mutate the loaded array");
        assert!(pdf.dirty_object_refs().is_empty());

        pdf.mark_object_dirty(pages_ref);
        assert_eq!(pdf.dirty_object_refs(), vec![pages_ref]);
    }

    #[test]
    fn insert_array_item_at_size_uses_qpdfs_append_position() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let appended_at_size = ObjectHandle::integer(2);

        array
            .insert_array_item(1, appended_at_size.clone())
            .expect("insertItem permits the inclusive end position");

        assert!(array
            .try_array_item(1)
            .unwrap()
            .unwrap()
            .is_same_object_as(&appended_at_size));
    }

    #[test]
    fn set_array_items_replaces_in_qpdf_order_and_preserves_child_identity() {
        let old = ObjectHandle::integer(1);
        let first = ObjectHandle::dictionary(vec![]);
        let second = ObjectHandle::dictionary(vec![]);
        let array = ObjectHandle::array(vec![old.clone()]);

        array
            .set_array_items(vec![first.clone(), second.clone()])
            .expect("setArrayFromVector accepts unowned items");

        assert!(old.containing_object_refs().is_empty());
        assert!(array
            .try_array_item(0)
            .unwrap()
            .unwrap()
            .is_same_object_as(&first));
        assert!(array
            .try_array_item(1)
            .unwrap()
            .unwrap()
            .is_same_object_as(&second));
        assert_eq!(array.try_array_len().unwrap(), Some(2));
    }

    #[test]
    fn array_mutators_reject_foreign_ownership_and_keep_qpdf_partial_replacement_order() {
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let old = ObjectHandle::integer(1);
        let array = ObjectHandle::array(vec![old.clone()]);
        array.promote_to_indirect(ObjectRef::new(40, 0), 41, Rc::downgrade(&resolver));

        let foreign = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(41, 0),
            NO_PARSED_OFFSET,
            42,
            Rc::downgrade(&resolver),
        );
        let error = array
            .append_array_item(foreign.clone())
            .expect_err("appendItem rejects an object owned by another PDF");
        assert!(matches!(error, Error::Internal(_)));
        assert_eq!(array.try_array_len().unwrap(), Some(1));
        assert!(array
            .try_array_item(0)
            .unwrap()
            .unwrap()
            .is_same_object_as(&old));

        let accepted = ObjectHandle::dictionary(vec![]);
        let error = array
            .set_array_items(vec![accepted.clone(), foreign])
            .expect_err("setFromVector checks ownership in insertion order");
        assert!(matches!(error, Error::Internal(_)));
        assert!(old.containing_object_refs().is_empty());
        assert_eq!(array.try_array_len().unwrap(), Some(1));
        assert!(array
            .try_array_item(0)
            .unwrap()
            .unwrap()
            .is_same_object_as(&accepted));

        let same_pdf = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(42, 0),
            NO_PARSED_OFFSET,
            41,
            Rc::downgrade(&resolver),
        );
        array
            .append_array_item(same_pdf.clone())
            .expect("same-document indirect items remain attachable");
        assert!(array
            .try_array_item(1)
            .unwrap()
            .unwrap()
            .is_same_object_as(&same_pdf));
    }

    #[test]
    fn canonical_array_mutators_resolve_a_lazy_holder_and_update_every_alias() {
        let child = ObjectHandle::integer(1);
        let (array, _resolver) =
            super::identity_tests::resolver_bearing_handle(ObjectValue::Array(vec![child.clone()]));
        let alias = array.clone();
        let replacement = ObjectHandle::integer(2);

        array
            .set_array_item(0, replacement.clone())
            .expect("qpdf array mutators dereference their holder");

        assert!(array.is_resolved());
        assert!(alias
            .try_array_item(0)
            .unwrap()
            .unwrap()
            .is_same_object_as(&replacement));
        assert!(child.containing_object_refs().is_empty());
    }

    #[test]
    fn array_mutators_reject_destroyed_items_as_uninitialized() {
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let array = ObjectHandle::array(vec![]);
        array.promote_to_indirect(ObjectRef::new(50, 0), 51, Rc::downgrade(&resolver));
        let destroyed = ObjectHandle::integer(1);
        destroyed.promote_to_indirect(ObjectRef::new(52, 0), 51, Rc::downgrade(&resolver));
        destroyed.disconnect();

        let error = array
            .append_array_item(destroyed)
            .expect_err("QPDF_Destroyed is not an initialized array item");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "Attempting to add an uninitialized object to a QPDF_Array."
        ));
        assert_eq!(array.try_array_len().unwrap(), Some(0));
    }

    #[test]
    fn replace_key_rejects_inserting_a_direct_dictionary_into_itself() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let self_clone = dict.clone();
        dict.replace_key(b"/Self", self_clone);
        assert!(dict.get_key(b"/Self").is_null());
        // The rest of the dictionary is untouched by the rejected insert.
        assert_eq!(dict.get_key(b"/A").as_integer(), Some(1));
    }

    #[test]
    fn replace_key_rejects_a_direct_alias_of_a_shared_payload() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(39, 0), -1);
        let replacement = ObjectHandle::dictionary(vec![]);
        target
            .share_value_state_with(&replacement)
            .expect("share replacement payload");

        target.replace_key(b"/Self", replacement.clone());

        assert!(!target.has_key(b"Self"));
    }

    #[test]
    fn replace_array_item_rejects_a_direct_alias_of_a_shared_payload() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(40, 0), -1);
        let replacement = ObjectHandle::array(vec![ObjectHandle::integer(2)]);
        target
            .share_value_state_with(&replacement)
            .expect("share replacement payload");

        assert!(!target.replace_array_item(0, replacement.clone()));
        assert_eq!(target.as_array().unwrap()[0].as_integer(), Some(2));
    }

    #[test]
    fn replace_array_items_rejects_a_direct_alias_of_a_shared_payload() {
        let target = ObjectHandle::new_indirect_unresolved(ObjectRef::new(41, 0), -1);
        let replacement = ObjectHandle::array(vec![ObjectHandle::integer(3)]);
        target
            .share_value_state_with(&replacement)
            .expect("share replacement payload");

        assert!(!target.replace_array_items(vec![replacement.clone()]));
        assert_eq!(target.as_array().unwrap()[0].as_integer(), Some(3));
    }

    #[test]
    fn replace_key_allows_an_indirect_handle_to_reference_itself() {
        // Unlike a direct self-insertion, an indirect handle referencing
        // itself is not a direct cycle -- every recursive walker already
        // stops at the indirect boundary, so this must remain a normal
        // insert rather than being rejected as a no-op.
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);
        indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
        indirect.replace_key(b"/Self", indirect.clone());
        assert!(indirect.get_key(b"/Self").is_indirect());
    }

    #[test]
    fn remove_key_deletes_a_present_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        dict.remove_key(b"/A");
        assert!(dict.get_key(b"/A").is_null());
    }

    #[test]
    fn remove_key_on_a_missing_key_is_a_no_op() {
        let dict = ObjectHandle::dictionary(vec![]);
        dict.remove_key(b"/Missing");
        assert!(dict.get_key(b"/Missing").is_null());
    }

    #[test]
    fn remove_key_on_a_non_dictionary_handle_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);
        scalar.remove_key(b"/A");
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn dictionary_detach_removes_only_the_removed_live_path() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let child = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(
            [
                (b"A".to_vec(), child.clone()),
                (b"B".to_vec(), child.clone()),
            ]
            .into_iter()
            .collect(),
        ));

        owner.remove_key(b"/A");
        assert_eq!(child.containing_object_refs(), vec![owner_ref]);
        owner.remove_key(b"/B");
        assert!(child.containing_object_refs().is_empty());
    }

    #[test]
    fn set_resolved_moves_dictionary_storage_without_cloning_it() {
        let owner = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);
        let mut key = vec![b'K'; 4_096];
        key[0] = b'/';
        let original_key_allocation = key.as_ptr();
        let value =
            ObjectValue::Dictionary([(key, ObjectHandle::integer(1))].into_iter().collect());

        owner.set_resolved(value);

        assert!(owner.is_indirect());
        let slot = owner.0.borrow();
        let state = slot.state.clone();
        drop(slot);
        let state = state.borrow();
        let ObjectState::Resolved(ObjectValue::Dictionary(entries)) = &*state else {
            panic!("test owner must resolve to the supplied dictionary"); // cov:ignore: successful set_resolved fixes this state
        };
        let resolved_key_allocation = entries
            .keys()
            .next()
            .expect("resolved dictionary retains its key")
            .as_ptr();
        assert_eq!(resolved_key_allocation, original_key_allocation);
    }

    #[test]
    fn expired_direct_parent_edges_are_pruned_on_attach_and_query() {
        fn parent_count(handle: &ObjectHandle) -> usize {
            assert!(handle.is_direct());
            handle.0.borrow().containment_parents.len()
        }

        let child = ObjectHandle::integer(1);
        for _ in 0..64 {
            let transient_parent = ObjectHandle::array(vec![child.clone()]);
            drop(transient_parent);
        }

        let live_parent = ObjectHandle::array(vec![child.clone()]);
        assert_eq!(parent_count(&child), 1);

        drop(live_parent);
        assert!(child.containing_object_refs().is_empty());
        assert_eq!(parent_count(&child), 0);
    }

    #[test]
    fn deep_containment_traversals_do_not_overflow_the_stack() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "object_handle::mutation_tests::deep_containment_traversals_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("FLPDF_DEEP_CONTAINMENT_PROBE", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "deep-containment probe failed: status={} stderr={}",
            output.status,
            stderr
        );
    }

    #[test]
    #[ignore = "subprocess-only stack-overflow regression probe"]
    fn deep_containment_traversals_probe() {
        assert_eq!(
            std::env::var_os("FLPDF_DEEP_CONTAINMENT_PROBE").as_deref(),
            Some(std::ffi::OsStr::new("1"))
        );

        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved_with_identity(
            owner_ref,
            NO_PARSED_OFFSET,
            Some(41),
            None,
        );
        let leaf = ObjectHandle::integer(1);
        let mut nested = leaf.clone();
        for _ in 0..100_000 {
            nested = ObjectHandle::array(vec![nested]);
        }

        owner.set_resolved(ObjectValue::Array(vec![nested.clone()]));
        assert_eq!(leaf.containing_object_refs_for_pdf(41), vec![owner_ref]);

        // These user-constructed values are intentionally deeper than Rust's
        // recursive Rc drop can safely release. Keep this probe scoped to the
        // two containment traversals under test.
        std::mem::forget(owner);
        std::mem::forget(nested);
    }

    #[test]
    fn replacing_a_nested_dictionary_path_detaches_the_old_subtree() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let leaf = ObjectHandle::integer(1);
        let nested = ObjectHandle::dictionary(vec![(b"Leaf".to_vec(), leaf.clone())]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Nested".to_vec(), nested.clone())].into_iter().collect(),
        ));

        owner.replace_key(b"/Nested", ObjectHandle::dictionary(vec![]));

        assert!(nested.containing_object_refs().is_empty());
        assert!(leaf.containing_object_refs().is_empty());
    }

    #[test]
    fn shared_subtree_loses_only_the_detached_indirect_root() {
        let first_ref = ObjectRef::new(7, 0);
        let second_ref = ObjectRef::new(9, 0);
        let first = ObjectHandle::new_indirect_unresolved(first_ref, -1);
        let second = ObjectHandle::new_indirect_unresolved(second_ref, -1);
        let shared = ObjectHandle::dictionary(vec![]);
        first.set_resolved(ObjectValue::Dictionary(
            [(b"Shared".to_vec(), shared.clone())].into_iter().collect(),
        ));
        second.set_resolved(ObjectValue::Dictionary(
            [(b"Shared".to_vec(), shared.clone())].into_iter().collect(),
        ));

        first.remove_key(b"/Shared");

        assert_eq!(shared.containing_object_refs(), vec![second_ref]);
    }

    #[test]
    fn array_and_direct_value_replacement_detach_old_children() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let first = ObjectHandle::integer(1);
        let second = ObjectHandle::integer(2);
        let array = ObjectHandle::array(vec![first.clone(), first.clone()]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Array".to_vec(), array.clone())].into_iter().collect(),
        ));

        assert!(array.replace_array_item(0, second.clone()));
        assert_eq!(first.containing_object_refs(), vec![owner_ref]);
        assert_eq!(second.containing_object_refs(), vec![owner_ref]);
        assert!(array.replace_array_items(vec![]));
        assert!(first.containing_object_refs().is_empty());
        assert!(second.containing_object_refs().is_empty());

        let replacement = ObjectHandle::integer(3);
        array.replace_direct_value(ObjectValue::Array(vec![replacement.clone()]));
        assert_eq!(replacement.containing_object_refs(), vec![owner_ref]);
        array.replace_direct_value(ObjectValue::Array(vec![]));
        assert!(replacement.containing_object_refs().is_empty());
    }

    #[test]
    fn stream_dictionary_membership_tracks_replacement_and_root_disconnect() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, NO_PARSED_OFFSET);
        let old_dictionary = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: old_dictionary.clone(),
            stream_data: None,
            stream_provider: None,
            stream_length: 0,
        });
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Stream".to_vec(), stream.clone())].into_iter().collect(),
        ));
        assert_eq!(old_dictionary.containing_object_refs(), vec![owner_ref]);

        let new_dictionary = ObjectHandle::dictionary(vec![]);
        stream.replace_direct_value(ObjectValue::Stream {
            stream_dict: new_dictionary.clone(),
            stream_data: None,
            stream_provider: None,
            stream_length: 0,
        });
        assert!(old_dictionary.containing_object_refs().is_empty());
        assert_eq!(new_dictionary.containing_object_refs(), vec![owner_ref]);

        owner.disconnect();
        assert!(new_dictionary.containing_object_refs().is_empty());
    }

    #[test]
    fn indirect_state_replacement_detaches_old_direct_children() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let replaced = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Child".to_vec(), replaced.clone())]
                .into_iter()
                .collect(),
        ));
        let missing = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Child".to_vec(), missing.clone())].into_iter().collect(),
        ));
        assert!(replaced.containing_object_refs().is_empty());
        assert_eq!(missing.containing_object_refs(), vec![owner_ref]);

        owner.set_missing();
        assert!(missing.containing_object_refs().is_empty());

        let disconnected = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Child".to_vec(), disconnected.clone())]
                .into_iter()
                .collect(),
        ));
        owner.disconnect();
        assert!(disconnected.containing_object_refs().is_empty());
    }

    #[test]
    fn current_root_lookup_terminates_on_a_direct_cycle() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let first = ObjectHandle::dictionary(vec![]);
        let second = ObjectHandle::dictionary(vec![]);
        first.replace_key(b"/Second", second.clone());
        second.replace_key(b"/First", first.clone());
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"First".to_vec(), first.clone())].into_iter().collect(),
        ));

        assert_eq!(second.containing_object_refs(), vec![owner_ref]);
    }

    #[test]
    fn shallow_copy_is_always_direct_even_from_an_indirect_source() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
        let copy = indirect.shallow_copy().expect("dictionary copy");
        assert!(copy.is_direct());
    }

    #[test]
    fn shallow_copy_mutation_does_not_affect_the_source() {
        let original = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let copy = original.shallow_copy().expect("dictionary copy");
        copy.replace_key(b"/A", ObjectHandle::integer(2));
        assert_eq!(original.get_key(b"/A").as_integer(), Some(1));
        assert_eq!(copy.get_key(b"/A").as_integer(), Some(2));
    }

    // Despite the name, qpdf's shallowCopy() recursively copies through
    // every *direct* array/dictionary descendant, stopping only at an
    // *indirect* child (kept shared) -- see shallow_copy's own doc comment
    // for the qpdf citations. These two tests pin that distinction.

    #[test]
    fn shallow_copy_of_a_direct_dictionary_child_produces_an_independent_copy() {
        let original = ObjectHandle::dictionary(vec![(
            b"A".to_vec(),
            ObjectHandle::dictionary(vec![(b"Inner".to_vec(), ObjectHandle::integer(1))]),
        )]);
        let copy = original.shallow_copy().expect("dictionary copy");
        copy.get_key(b"/A")
            .replace_key(b"/Inner", ObjectHandle::integer(2));
        assert_eq!(
            original.get_key(b"/A").get_key(b"/Inner").as_integer(),
            Some(1)
        );
        assert_eq!(copy.get_key(b"/A").get_key(b"/Inner").as_integer(), Some(2));
    }

    #[test]
    fn shallow_copy_of_an_indirect_dictionary_child_keeps_shared_identity() {
        let child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        child.set_resolved(ObjectValue::Integer(1));
        let original = ObjectHandle::dictionary(vec![(b"A".to_vec(), child.clone())]);
        let copy = original.shallow_copy().expect("dictionary copy");
        assert!(copy.get_key(b"/A").ptr_eq(&child));
    }

    #[test]
    fn shallow_copy_of_a_non_container_clones_the_scalar_value() {
        let original = ObjectHandle::integer(5);
        let copy = original.shallow_copy().expect("scalar copy");
        assert!(!copy.ptr_eq(&original));
        assert_eq!(copy.as_integer(), Some(5));
    }

    #[test]
    fn has_key_distinguishes_a_present_null_value_from_a_missing_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        assert!(dict.has_key(b"/A"));
        assert!(!dict.has_key(b"/Missing"));
    }

    #[test]
    fn has_key_on_a_non_dictionary_handle_is_false() {
        let scalar = ObjectHandle::integer(1);
        assert!(!scalar.has_key(b"/A"));
    }

    #[test]
    fn merge_resources_is_a_no_op_unless_both_sides_are_dictionaries() {
        let scalar = ObjectHandle::integer(1);
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        scalar.merge_resources(&dict, None).expect("merge");
        dict.merge_resources(&scalar, None).expect("merge");
        assert_eq!(dict.get_key(b"/A").as_integer(), Some(1));
        assert!(dict.get_key(b"/B").is_null());
    }

    // qpdf privatizes an incoming resource value with `shallowCopy`
    // (`libqpdf/QPDFObjectHandle.cc:1090`, `:1113`, `:1122`), so a *direct*
    // stream anywhere it reaches propagates `QPDF_Stream::copy`'s throw out
    // of `mergeResources` rather than being merged. Both privatizing sites
    // are covered: a whole rtype `self` lacks, and an inner key inside a
    // shared rtype.
    #[test]
    fn merge_resources_propagates_the_stream_rejection_from_either_privatizing_site() {
        let stream = || {
            ObjectHandle::stream(
                ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(3))]),
                Rc::new(b"abc".to_vec()),
            )
        };

        let new_rtype_dest = ObjectHandle::dictionary(vec![]);
        let new_rtype_other = ObjectHandle::dictionary(vec![(
            b"XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"Im1".to_vec(), stream())]),
        )]);

        let inner_key_dest = ObjectHandle::dictionary(vec![(
            b"XObject".to_vec(),
            ObjectHandle::dictionary(vec![]),
        )]);
        let inner_key_other = ObjectHandle::dictionary(vec![(
            b"XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"Im1".to_vec(), stream())]),
        )]);

        for (dest, other) in [
            (new_rtype_dest, new_rtype_other),
            (inner_key_dest, inner_key_other),
        ] {
            let error = dest
                .merge_resources(&other, None)
                .expect_err("a direct stream resource cannot be privatized");
            assert!(
                matches!(error, Error::System(ref message)
                    if message == "stream objects cannot be cloned"),
                "{error:?}"
            );
        }
    }

    #[test]
    fn merge_resources_installs_a_private_copy_of_a_top_level_key_self_lacks() {
        let source_sub = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), source_sub.clone())]);
        let dest = ObjectHandle::dictionary(vec![]);
        dest.merge_resources(&other, None).expect("merge");
        let installed = dest.get_key(b"/Font");
        assert_eq!(installed.get_key(b"/F1").as_integer(), Some(1));
        assert!(!installed.ptr_eq(&source_sub)); // privatized, not shared
    }

    #[test]
    fn merge_resources_adds_a_new_inner_key_without_a_conflicts_map() {
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F2".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        dest.merge_resources(&other, None).expect("merge");
        let font = dest.get_key(b"/Font");
        assert_eq!(font.get_key(b"/F1").as_integer(), Some(1));
        assert_eq!(font.get_key(b"/F2").as_integer(), Some(2));
    }

    #[test]
    fn merge_resources_leaves_a_colliding_inner_key_untouched_without_a_conflicts_map() {
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font =
            ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(99))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        dest.merge_resources(&other, None).expect("merge");
        assert_eq!(dest.get_key(b"/Font").get_key(b"/F1").as_integer(), Some(1));
    }

    #[test]
    fn merge_resources_reuses_an_existing_key_for_the_same_indirect_object() {
        let shared = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        shared.set_resolved(ObjectValue::Name(b"Shared".to_vec()));
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        // dest already has F1 -> shared. other also wants F1, but pointing at
        // the same shared object identity -- reuse F1 verbatim, no conflict
        // entry (existing_key == key), no new key minted.
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts))
            .expect("merge");
        assert!(conflicts.is_empty());
        assert!(dest.get_key(b"/Font").get_key(b"/F1").ptr_eq(&shared));
    }

    #[test]
    fn merge_resources_reuse_records_a_conflict_when_the_reused_name_differs() {
        let shared = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        shared.set_resolved(ObjectValue::Name(b"Shared".to_vec()));
        // dest already has this same shared object, but under a DIFFERENT
        // key (F2) than what other asks for (F1) -- reuse F2, and DO record
        // the rename since the reused name differs from the requested one.
        let this_font = ObjectHandle::dictionary(vec![
            (b"F2".to_vec(), shared.clone()),
            (b"F1".to_vec(), ObjectHandle::integer(1)),
        ]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts))
            .expect("merge");
        assert_eq!(
            conflicts
                .get(b"/Font".as_slice())
                .unwrap()
                .get(b"/F1".as_slice()),
            Some(&b"/F2".to_vec())
        );
        // F1 keeps its own original (unrelated) value; nothing overwrote it.
        assert_eq!(dest.get_key(b"/Font").get_key(b"/F1").as_integer(), Some(1));
    }

    #[test]
    fn merge_resources_mints_a_fresh_name_for_a_genuine_conflict() {
        let this_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(1))]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts))
            .expect("merge");
        let new_name = conflicts
            .get(b"/Font".as_slice())
            .and_then(|m| m.get(b"/F1".as_slice()))
            .expect("F1 conflict recorded");
        assert_eq!(new_name, b"/F1_1");
        assert_eq!(dest.get_key(b"/Font").get_key(b"/F1").as_integer(), Some(1));
        assert_eq!(
            dest.get_key(b"/Font").get_key(new_name).as_integer(),
            Some(2)
        );
    }

    #[test]
    fn merge_resources_privatizes_an_indirect_existing_sub_dictionary() {
        let indirect_font = ObjectHandle::new_indirect_unresolved(ObjectRef::new(3, 0), -1);
        indirect_font.set_resolved(ObjectValue::Dictionary(
            [(b"F1".to_vec(), ObjectHandle::integer(1))]
                .into_iter()
                .collect(),
        ));
        let shared_dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), indirect_font.clone())]);
        let another_holder =
            ObjectHandle::dictionary(vec![(b"Font".to_vec(), indirect_font.clone())]);
        let other_font = ObjectHandle::dictionary(vec![(b"F2".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        shared_dest.merge_resources(&other, None).expect("merge");
        // shared_dest's own /Font is now a private direct copy...
        assert!(shared_dest.get_key(b"/Font").is_direct());
        assert_eq!(
            shared_dest.get_key(b"/Font").get_key(b"/F2").as_integer(),
            Some(2)
        );
        // ...and the other holder's /Font (and the original indirect object)
        // is untouched.
        assert!(another_holder.get_key(b"/Font").ptr_eq(&indirect_font));
        assert!(indirect_font.get_key(b"/F2").is_null());
    }

    #[test]
    fn merge_resources_unions_scalar_array_items_by_unparsed_text() {
        let dest = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )]);
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"PDF".to_vec()),
                ObjectHandle::name(b"Text".to_vec()),
            ]),
        )]);
        dest.merge_resources(&other, None).expect("merge");
        let items = dest.get_key(b"/ProcSet").as_array().unwrap();
        let names: Vec<_> = items.iter().map(|i| i.as_name().unwrap()).collect();
        assert_eq!(names, vec![b"PDF".to_vec(), b"Text".to_vec()]);
    }

    #[test]
    fn merge_resources_array_union_records_the_destination_owner() {
        let owner_ref = ObjectRef::new(7, 0);
        let owner =
            ObjectHandle::new_indirect_unresolved_with_identity(owner_ref, -1, Some(41), None);
        let dest = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
        )]);
        owner.set_resolved(ObjectValue::Dictionary(
            [(b"Resources".to_vec(), dest.clone())]
                .into_iter()
                .collect(),
        ));
        let retained = ObjectHandle::name(b"Text".to_vec());
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![retained.clone()]),
        )]);

        dest.merge_resources(&other, None).expect("merge");

        let merged = dest
            .get_key(b"/ProcSet")
            .as_array()
            .expect("merged ProcSet remains an array");
        assert!(merged[1].is_same_object_as(&retained));
        assert_eq!(retained.containing_object_refs_for_pdf(41), vec![owner_ref]);
    }

    #[test]
    fn merge_resources_leaves_mismatched_or_non_container_rtype_shapes_untouched() {
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), ObjectHandle::integer(1))]);
        let other = ObjectHandle::dictionary(vec![(
            b"Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]),
        )]);
        dest.merge_resources(&other, None).expect("merge");
        assert_eq!(dest.get_key(b"/Font").as_integer(), Some(1));
    }

    #[test]
    fn replace_stream_data_updates_data_and_length() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: None,
            stream_provider: None,
            stream_length: 37,
        });
        stream.replace_stream_data(Rc::new(b"new data".to_vec()), None, None);
        assert_eq!(stream.as_stream_data(), Some(Rc::new(b"new data".to_vec())));
        assert_eq!(dict.get_key(b"/Length").as_integer(), Some(8));
        assert_eq!(
            stream.with_value(|value| match value {
                Some(ObjectValue::Stream { stream_length, .. }) => Some(*stream_length),
                _ => None, // cov:ignore: this test constructs a stream above
            }),
            Some(37),
            "replaceStreamData changes the dictionary /Length, not the stored original length"
        );
    }

    #[test]
    fn replace_stream_data_empty_buffer_removes_an_existing_length() {
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(37))]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            stream_length: 37,
        });

        stream.replace_stream_data(Rc::new(Vec::new()), None, None);

        assert!(!dict.has_key(b"/Length"));
    }

    #[test]
    fn replace_stream_data_empty_buffer_keeps_a_missing_length_absent() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            stream_length: 3,
        });

        stream.replace_stream_data(Rc::new(Vec::new()), None, None);

        assert!(!dict.has_key(b"/Length"));
    }

    #[test]
    fn replace_stream_data_repeated_empty_and_nonempty_calls_follow_the_length_boundary() {
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(3))]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            stream_length: 3,
        });

        stream.replace_stream_data(Rc::new(Vec::new()), None, None);
        assert!(!dict.has_key(b"/Length"));

        stream.replace_stream_data(Rc::new(b"new data".to_vec()), None, None);
        assert_eq!(dict.get_key(b"/Length").as_integer(), Some(8));

        stream.replace_stream_data(Rc::new(Vec::new()), None, None);
        assert!(!dict.has_key(b"/Length"));
    }

    #[test]
    fn replace_stream_data_mutates_a_document_owned_indirect_stream_dictionary() {
        let mut pdf = crate::Pdf::empty().expect("empty PDF");
        let stream_ref = ObjectRef::new(3, 0);
        let mut dict = crate::Dictionary::new();
        dict.insert("Length", crate::Object::Integer(3));
        pdf.set_object(
            stream_ref,
            crate::Object::Stream(crate::Stream::new(dict, b"old".to_vec())),
        );
        let stream = pdf.get_object_handle(stream_ref);
        stream.try_dereference().expect("document-owned stream");

        stream.replace_stream_data(Rc::new(Vec::new()), None, None);

        assert!(stream.is_indirect());
        assert_eq!(stream.object_ref(), Some(stream_ref));
        assert!(!stream
            .as_stream_dict()
            .expect("stream dictionary")
            .has_key(b"/Length"));
    }

    #[test]
    fn replace_stream_data_sets_filter_and_decode_parms_when_given() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        let filter = ObjectHandle::name(b"FlateDecode".to_vec());
        let parms =
            ObjectHandle::dictionary(vec![(b"Predictor".to_vec(), ObjectHandle::integer(12))]);
        stream.replace_stream_data(
            Rc::new(b"x".to_vec()),
            Some(filter.clone()),
            Some(parms.clone()),
        );
        assert!(dict.get_key(b"/Filter").ptr_eq(&filter));
        assert!(dict.get_key(b"/DecodeParms").ptr_eq(&parms));
    }

    #[test]
    fn replace_stream_data_leaves_filter_untouched_when_not_given() {
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });
        stream.replace_stream_data(Rc::new(b"new".to_vec()), None, None);
        assert_eq!(
            dict.get_key(b"/Filter").as_name(),
            Some(b"FlateDecode".to_vec())
        );
    }

    #[test]
    fn replace_stream_data_on_a_non_stream_handle_is_a_no_op() {
        let scalar = ObjectHandle::integer(1);
        scalar.replace_stream_data(Rc::new(b"x".to_vec()), None, None);
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn raw_stream_data_rejects_an_original_stream_with_parsed_offset_zero() {
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![]),
            stream_data: None,
            stream_provider: None,
            stream_length: 3,
        });
        stream.set_parsed_offset_if_unset(0);

        let error = stream
            .get_raw_stream_data()
            .expect_err("qpdf rejects an original stream with no parsed offset");
        assert!(matches!(error, Error::Internal(message)
            if message == "pipeStreamData called for stream with no data"));
    }

    #[test]
    fn raw_stream_data_rejects_non_stream_values() {
        let error = ObjectHandle::integer(1)
            .get_raw_stream_data()
            .expect_err("raw stream data is only available on streams");
        assert!(matches!(error, Error::Internal(message)
            if message == "pipeStreamData called for non-stream"));
    }

    #[test]
    fn raw_stream_data_rejects_an_original_direct_stream() {
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![]),
            stream_data: None,
            stream_provider: None,
            stream_length: 3,
        });

        let error = stream
            .get_raw_stream_data()
            .expect_err("qpdf streams with original bytes are always indirect");
        assert!(matches!(error, Error::Internal(message)
            if message == "pipeStreamData called for original direct stream"));
    }

    #[test]
    fn raw_stream_data_rejects_a_dropped_original_stream_owner() {
        let (stream, resolver) =
            super::identity_tests::resolver_bearing_handle(ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![]),
                stream_data: None,
                stream_provider: None,
                stream_length: 3,
            });
        stream.set_parsed_offset_if_unset(9);
        stream
            .try_dereference()
            .expect("resolve stream while owner lives");
        drop(resolver);

        let error = stream
            .get_raw_stream_data()
            .expect_err("original stream source is unavailable after document drop");
        assert!(matches!(error, Error::Internal(message)
            if message == "object 20 0 belongs to a dropped PDF"));
    }

    // --- Coverage closers: paths the tests above never happened to reach ---

    #[test]
    fn replace_key_and_remove_key_mutate_a_resolved_indirect_handle() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.set_resolved(ObjectValue::Dictionary(Default::default()));
        indirect.replace_key(b"/A", ObjectHandle::integer(1));
        assert_eq!(indirect.get_key(b"/A").as_integer(), Some(1));
        indirect.remove_key(b"/A");
        assert!(indirect.get_key(b"/A").is_null());
    }

    #[test]
    fn resolving_an_indirect_dictionary_records_its_direct_child_owner() {
        // This fails if resolution leaves a direct child detached from the
        // canonical indirect object that contains it. Pdf's incremental
        // writer then has no local owner to schedule after the child mutates.
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let child = ObjectHandle::dictionary(vec![]);

        owner.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Child".to_vec(), child.clone()),
        ])));

        assert_eq!(child.containing_object_refs(), vec![owner_ref]);
    }

    #[test]
    fn an_indirect_handle_has_no_direct_containment_owner() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);

        assert!(handle.containing_object_refs_for_pdf(1).is_empty());
    }

    #[test]
    fn replacing_a_contained_direct_value_propagates_its_owner_to_new_children() {
        // A preserved direct stream dictionary is replaced in place by
        // Pdf::set_object. New direct descendants must inherit the same
        // incremental-write owner rather than requiring a later graph scan.
        let owner_ref = ObjectRef::new(7, 0);
        let owner = ObjectHandle::new_indirect_unresolved(owner_ref, -1);
        let direct = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Direct".to_vec(), direct.clone()),
        ])));
        let child = ObjectHandle::integer(42);

        direct.replace_direct_value(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Child".to_vec(), child.clone()),
        ])));

        assert_eq!(child.containing_object_refs(), vec![owner_ref]);
    }

    #[test]
    fn associating_direct_owners_stops_at_an_indirect_child() {
        // Direct containment ends at indirect identity. Propagating owner 7
        // through this boundary would incorrectly make object 9's payload a
        // direct child of object 7.
        let owner = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 0), -1);
        let direct = ObjectHandle::dictionary(vec![]);
        owner.set_resolved(ObjectValue::Dictionary(std::collections::BTreeMap::from([
            (b"Direct".to_vec(), direct.clone()),
        ])));
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);

        direct.replace_key(b"/Indirect", indirect.clone());

        assert!(direct.get_key(b"/Indirect").is_same_object_as(&indirect));
        assert!(indirect.containing_object_refs().is_empty());
    }

    #[test]
    fn replace_key_and_remove_key_are_no_ops_on_an_unresolved_indirect_handle() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.replace_key(b"/A", ObjectHandle::integer(1)); // must not panic
        indirect.remove_key(b"/A"); // must not panic
        assert!(indirect.get_key(b"/A").is_null());
    }

    #[test]
    fn shallow_copy_of_an_unresolved_indirect_handle_is_a_direct_null() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        let copy = indirect.shallow_copy().expect("unresolved copy");
        assert!(copy.is_direct());
        assert!(copy.is_null());
    }

    #[test]
    fn shallow_copy_of_an_array_recurses_through_direct_elements() {
        let inner = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let original = ObjectHandle::array(vec![inner]);
        let copy = original.shallow_copy().expect("array copy");
        let copy_inner = copy.as_array().unwrap()[0].clone();
        assert!(!copy_inner.ptr_eq(&original.as_array().unwrap()[0]));
        assert_eq!(
            copy.as_array().unwrap()[0].as_array().unwrap()[0].as_integer(),
            Some(1)
        );
    }

    #[test]
    fn shallow_copy_refuses_a_resolved_indirect_stream() {
        // `shallowCopy` dereferences first (`libqpdf/QPDFObjectHandle.cc:2074-2078`)
        // and only then calls `obj->copy()`, so an indirect handle whose
        // resolved value is a stream reaches `QPDF_Stream::copy`'s throw just
        // as a direct one does — the resolution state changes nothing.
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        indirect.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![]),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            stream_length: 0,
        });

        let error = indirect
            .shallow_copy()
            .expect_err("streams cannot be cloned");

        assert!(
            matches!(error, Error::System(ref message)
                if message == "stream objects cannot be cloned"),
            "{error:?}"
        );
        assert_eq!(indirect.as_stream_data(), Some(Rc::new(b"old".to_vec())));
    }

    #[test]
    fn merge_resources_installs_an_already_indirect_new_key_without_shallow_copying() {
        let shared = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), -1);
        shared.set_resolved(ObjectValue::Integer(1));
        let this_font = ObjectHandle::dictionary(vec![]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), shared.clone())]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        dest.merge_resources(&other, None).expect("merge");
        assert!(dest.get_key(b"/Font").get_key(b"/F1").ptr_eq(&shared));
    }

    #[test]
    fn merge_resources_array_union_skips_a_non_scalar_item() {
        let dest =
            ObjectHandle::dictionary(vec![(b"ProcSet".to_vec(), ObjectHandle::array(vec![]))]);
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::dictionary(vec![])]),
        )]);
        dest.merge_resources(&other, None).expect("merge");
        assert!(dest.get_key(b"/ProcSet").as_array().unwrap().is_empty());
    }

    #[test]
    fn is_scalar_covers_every_disjunct() {
        assert!(is_scalar(&ObjectHandle::boolean(true)));
        assert!(is_scalar(&ObjectHandle::integer(1)));
        assert!(is_scalar(&ObjectHandle::name(b"N".to_vec())));
        assert!(is_scalar(&ObjectHandle::null()));
        assert!(is_scalar(&ObjectHandle::real(1.0)));
        assert!(is_scalar(&ObjectHandle::string(b"S".to_vec())));
        assert!(!is_scalar(&ObjectHandle::array(vec![])));
    }

    #[test]
    fn merge_resources_mints_a_second_unique_name_when_the_first_candidate_is_taken() {
        // this_val (the Font sub-dict itself) has a nested dictionary-valued
        // entry ("Widths") whose own key happens to be "F1_1" --
        // get_resource_names is called ON this_val (see merge_resources's
        // own doc comment on why it is this level, not dest's), so its
        // "grandchildren" pool picks this up, forcing unique_resource_name
        // past its first candidate.
        let this_font = ObjectHandle::dictionary(vec![
            (b"F1".to_vec(), ObjectHandle::integer(1)),
            (
                b"Widths".to_vec(),
                ObjectHandle::dictionary(vec![(b"F1_1".to_vec(), ObjectHandle::integer(0))]),
            ),
        ]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();
        dest.merge_resources(&other, Some(&mut conflicts))
            .expect("merge");
        let new_name = conflicts
            .get(b"/Font".as_slice())
            .and_then(|m| m.get(b"/F1".as_slice()))
            .expect("F1 conflict recorded");
        assert_eq!(new_name, b"/F1_2");
        assert_eq!(
            dest.get_key(b"/Font").get_key(new_name).as_integer(),
            Some(2)
        );
    }
}

#[cfg(test)]
mod stream_provider_contract_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct LegacyProvider {
        calls: Cell<usize>,
        identities: RefCell<Vec<(u32, u16)>>,
    }

    impl StreamDataProvider for LegacyProvider {
        fn provide_stream_data_by_id(
            &self,
            object_number: u32,
            generation: u16,
            _pipeline: &mut dyn Pipeline,
        ) -> Result<()> {
            self.calls.set(self.calls.get() + 1);
            self.identities
                .borrow_mut()
                .push((object_number, generation));
            Ok(())
        }
    }

    #[derive(Default)]
    struct RetryProvider {
        calls: Cell<usize>,
        flags: RefCell<Vec<(bool, bool)>>,
    }

    impl StreamDataProvider for RetryProvider {
        fn supports_retry(&self) -> bool {
            true
        }

        fn provide_stream_data_with_retry_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            _pipeline: &mut dyn Pipeline,
            suppress_warnings: bool,
            will_retry: bool,
        ) -> Result<bool> {
            self.calls.set(self.calls.get() + 1);
            self.flags
                .borrow_mut()
                .push((suppress_warnings, will_retry));
            Ok(true)
        }
    }

    struct EmptyProvider;

    impl StreamDataProvider for EmptyProvider {}

    struct PipeProvider {
        bytes: Rc<Vec<u8>>,
        calls: Cell<usize>,
        identities: RefCell<Vec<(u32, u16)>>,
    }

    impl PipeProvider {
        fn new(bytes: Rc<Vec<u8>>) -> Self {
            Self {
                bytes,
                calls: Cell::new(0),
                identities: RefCell::new(Vec::new()),
            }
        }
    }

    impl StreamDataProvider for PipeProvider {
        fn provide_stream_data_by_id(
            &self,
            object_number: u32,
            generation: u16,
            pipeline: &mut dyn Pipeline,
        ) -> Result<()> {
            self.calls.set(self.calls.get() + 1);
            self.identities
                .borrow_mut()
                .push((object_number, generation));
            pipeline.write(&self.bytes).map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)
        }
    }

    struct RetryPipeProvider {
        bytes: Rc<Vec<u8>>,
        success: bool,
        flags: RefCell<Vec<(bool, bool)>>,
    }

    impl StreamDataProvider for RetryPipeProvider {
        fn supports_retry(&self) -> bool {
            true
        }

        fn provide_stream_data_with_retry_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            pipeline: &mut dyn Pipeline,
            suppress_warnings: bool,
            will_retry: bool,
        ) -> Result<bool> {
            self.flags
                .borrow_mut()
                .push((suppress_warnings, will_retry));
            pipeline.write(&self.bytes).map_err(Error::from)?;
            pipeline.finish().map_err(Error::from)?;
            Ok(self.success)
        }
    }

    fn provider_stream() -> ObjectHandle {
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(3))]),
            Rc::new(b"old".to_vec()),
        )
    }

    #[test]
    fn provider_registration_is_lazy_and_retains_the_provider_allocation() {
        let stream = provider_stream();
        let provider = Rc::new(LegacyProvider::default());
        let ownership = Rc::downgrade(&provider);

        stream
            .replace_stream_data_provider(provider.clone(), None, None)
            .expect("stream provider replacement");

        assert_eq!(provider.calls.get(), 0, "registration must be lazy");
        assert!(
            stream.as_stream_data().is_none(),
            "provider clears buffer source"
        );
        assert!(stream.with_value(|value| matches!(
            value,
            Some(ObjectValue::Stream {
                stream_provider: Some(_),
                ..
            })
        )));

        drop(provider);
        assert!(
            ownership.upgrade().is_some(),
            "stream must retain provider ownership"
        );
    }

    #[test]
    fn provider_registration_accepts_a_qpdf_owned_indirect_stream() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let object_ref = stream.object_ref().expect("new stream object identity");

        stream
            .replace_stream_data_provider(Rc::new(LegacyProvider::default()), None, None)
            .expect("provider replacement");

        assert!(stream.is_indirect());
        assert_eq!(stream.object_ref(), Some(object_ref));
        assert!(stream.as_stream_data().is_none());
        assert!(stream.with_value(|value| matches!(
            value,
            Some(ObjectValue::Stream {
                stream_provider: Some(_),
                ..
            })
        )));
    }

    #[test]
    fn provider_source_is_piped_lazily_and_updates_the_measured_length() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let object_ref = stream.object_ref().expect("new stream object identity");
        let bytes = Rc::new(b"provider bytes".to_vec());
        let provider = Rc::new(PipeProvider::new(Rc::clone(&bytes)));

        stream
            .replace_stream_data_provider(provider.clone(), None, None)
            .expect("provider replacement");
        assert_eq!(provider.calls.get(), 0, "provider remains lazy");

        let first = stream.get_raw_stream_data().expect("first provider pipe");
        let second = stream.get_raw_stream_data().expect("second provider pipe");
        assert_eq!(first.as_slice(), bytes.as_slice());
        assert_eq!(second.as_slice(), bytes.as_slice());
        assert_eq!(provider.calls.get(), 2);
        assert_eq!(
            *provider.identities.borrow(),
            vec![(object_ref.number, object_ref.generation); 2]
        );
        assert_eq!(
            stream
                .as_stream_dict()
                .expect("stream dictionary")
                .get_key(b"/Length")
                .as_integer(),
            Some(bytes.len() as i64)
        );
    }

    #[test]
    fn provider_retry_result_forwards_flags_and_skips_length_update_on_failure() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let bytes = Rc::new(b"retry bytes".to_vec());
        let provider = Rc::new(RetryPipeProvider {
            bytes: Rc::clone(&bytes),
            success: false,
            flags: RefCell::new(Vec::new()),
        });
        stream
            .replace_stream_data_provider(provider.clone(), None, None)
            .expect("provider replacement");

        let mut sink = crate::pipeline::buffer::Buffer::new("provider", None);
        let mut filtering_attempted = false;
        let success = stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::None,
                true,
                true,
            )
            .expect("retry provider result");

        assert!(!success);
        assert!(!filtering_attempted);
        assert_eq!(sink.take_buffer().expect("provider output"), *bytes);
        assert_eq!(*provider.flags.borrow(), vec![(true, true)]);
        assert!(!stream
            .as_stream_dict()
            .expect("stream dictionary")
            .has_key(b"/Length"));
    }

    #[test]
    fn provider_length_mismatch_uses_qpdf_system_error_text() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let object_ref = stream.object_ref().expect("new stream object identity");
        let bytes = Rc::new(b"provider bytes".to_vec());
        stream
            .replace_stream_data_provider(Rc::new(PipeProvider::new(Rc::clone(&bytes))), None, None)
            .expect("provider replacement");
        stream
            .as_stream_dict()
            .expect("stream dictionary")
            .replace_key(b"/Length", ObjectHandle::integer(99));

        let error = stream
            .get_raw_stream_data()
            .expect_err("provider length mismatch must fail");
        assert!(matches!(
            error,
            Error::System(message)
                if message == format!(
                    "stream data provider for {} {} provided {} bytes instead of expected 99 bytes",
                    object_ref.number,
                    object_ref.generation,
                    bytes.len()
                )
        ));
    }

    #[test]
    fn provider_source_reuses_the_filter_pipeline() {
        let decoded = b"decoded provider bytes";
        let mut filter_dict = Dictionary::new();
        filter_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = Rc::new(
            crate::filters::encode_stream_data(&filter_dict, decoded)
                .expect("provider source encoding"),
        );
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        stream
            .replace_stream_data_provider(
                Rc::new(PipeProvider::new(Rc::clone(&encoded))),
                Some(ObjectHandle::name(b"FlateDecode".to_vec())),
                None,
            )
            .expect("provider replacement");

        let output = stream
            .get_stream_data(crate::writer::DecodeLevel::Generalized)
            .expect("provider filter pipeline");
        assert_eq!(output.as_slice(), decoded);
        assert_eq!(
            stream
                .as_stream_dict()
                .expect("stream dictionary")
                .get_key(b"/Length")
                .as_integer(),
            Some(encoded.len() as i64)
        );
    }

    #[test]
    fn provider_default_error_propagates_from_the_pipe_boundary() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        stream
            .replace_stream_data_provider(Rc::new(EmptyProvider), None, None)
            .expect("provider replacement");

        let error = stream
            .get_raw_stream_data()
            .expect_err("provider default method must fail");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "you must override provideStreamData -- see QPDFObjectHandle.hh"
        ));
    }

    #[test]
    fn provider_pipe_rejects_a_direct_stream_without_object_identity() {
        let stream = provider_stream();
        stream
            .replace_stream_data_provider(
                Rc::new(PipeProvider::new(Rc::new(b"direct bytes".to_vec()))),
                None,
                None,
            )
            .expect("provider replacement");

        let error = stream
            .get_raw_stream_data()
            .expect_err("provider requires an indirect stream identity");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "pipeStreamData called for provider-backed direct stream"
        ));
    }

    #[test]
    fn callback_adapter_writes_through_the_provider_source_boundary() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        stream
            .replace_stream_data_with_callback(
                move |pipeline| {
                    callback_calls.set(callback_calls.get() + 1);
                    pipeline.write(b"callback bytes").map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)
                },
                None,
                None,
            )
            .expect("callback provider replacement");

        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("callback provider pipe")
                .as_slice(),
            b"callback bytes"
        );
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn replacing_a_provider_with_a_buffer_releases_the_provider_source() {
        let stream = provider_stream();
        let provider = Rc::new(LegacyProvider::default());
        let ownership = Rc::downgrade(&provider);

        stream
            .replace_stream_data_provider(provider.clone(), None, None)
            .expect("provider replacement");
        drop(provider);
        assert!(ownership.upgrade().is_some());

        stream.replace_stream_data(Rc::new(b"new".to_vec()), None, None);
        assert!(
            ownership.upgrade().is_none(),
            "buffer replacement must clear provider ownership"
        );
        assert_eq!(
            stream.as_stream_data().expect("buffer source").as_slice(),
            b"new"
        );
        assert!(stream.with_value(|value| matches!(
            value,
            Some(ObjectValue::Stream {
                stream_provider: None,
                ..
            })
        )));
    }

    #[test]
    fn provider_replacement_uses_qpdf_filter_boundary_for_uninitialized_and_null_values() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), ObjectHandle::name(b"Keep".to_vec())),
            (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let stream = ObjectHandle::stream(dict.clone(), Rc::new(b"old".to_vec()));

        stream
            .replace_stream_data_provider(Rc::new(LegacyProvider::default()), None, None)
            .expect("provider replacement");
        assert!(dict.has_key(b"/Filter"));
        assert!(dict.has_key(b"/DecodeParms"));
        assert!(!dict.has_key(b"/Length"));

        stream
            .replace_stream_data_provider(
                Rc::new(LegacyProvider::default()),
                Some(ObjectHandle::null()),
                Some(ObjectHandle::null()),
            )
            .expect("provider replacement with explicit nulls");
        assert!(!dict.has_key(b"/Filter"));
        assert!(!dict.has_key(b"/DecodeParms"));
    }

    #[test]
    fn provider_replacement_rejects_a_non_stream_with_qpdf_assertion_error() {
        let error = ObjectHandle::integer(7)
            .replace_stream_data_provider(Rc::new(LegacyProvider::default()), None, None)
            .expect_err("non-stream provider replacement must fail");

        assert!(matches!(
            error,
            Error::System(message)
                if message == "operation for stream attempted on object of type integer"
        ));
    }

    #[test]
    fn legacy_object_identity_form_delegates_to_the_numeric_form() {
        let provider = LegacyProvider::default();
        let mut sink = crate::pipeline::buffer::Buffer::new("provider", None);

        provider
            .provide_stream_data(ObjectRef::new(17, 4), &mut sink)
            .expect("legacy provider");

        assert_eq!(provider.calls.get(), 1);
        assert_eq!(*provider.identities.borrow(), vec![(17, 4)]);
    }

    #[test]
    fn retry_object_identity_form_delegates_flags_to_the_numeric_form() {
        let provider = RetryProvider::default();
        let mut sink = crate::pipeline::buffer::Buffer::new("provider", None);

        assert!(provider
            .provide_stream_data_with_retry(ObjectRef::new(23, 2), &mut sink, true, false,)
            .expect("retry provider"));
        assert!(provider.supports_retry());
        assert_eq!(provider.calls.get(), 1);
        assert_eq!(*provider.flags.borrow(), vec![(true, false)]);
    }

    #[test]
    fn default_provider_methods_return_qpdf_contract_error() {
        let provider = EmptyProvider;
        let mut sink = crate::pipeline::buffer::Buffer::new("provider", None);

        assert!(!provider.supports_retry());

        let error = provider
            .provide_stream_data(ObjectRef::new(1, 0), &mut sink)
            .expect_err("default provider must reject missing implementation");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "you must override provideStreamData -- see QPDFObjectHandle.hh"
        ));

        let error = provider
            .provide_stream_data_with_retry(ObjectRef::new(1, 0), &mut sink, false, true)
            .expect_err("default retry provider must reject missing implementation");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "you must override provideStreamData -- see QPDFObjectHandle.hh"
        ));
    }

    #[test]
    fn provider_debug_uses_qpdf_shaped_marker() {
        let provider: Rc<dyn StreamDataProvider> = Rc::new(EmptyProvider);

        assert_eq!(format!("{provider:?}"), "StreamDataProvider(..)");
    }

    fn registered_provider(value: Option<&ObjectValue>) -> Rc<dyn StreamDataProvider> {
        match value {
            Some(ObjectValue::Stream {
                stream_provider: Some(provider),
                ..
            }) => provider.clone(),
            _ => unreachable!("callback test stream must retain its provider"), // cov:ignore: test fixture always registers a provider-backed stream
        }
    }

    #[test]
    fn callback_adapters_forward_pipeline_data_and_retry_flags() {
        let mut void_sink = crate::pipeline::buffer::Buffer::new("void callback", None);
        let void_provider = CallbackProvider {
            callback: |pipeline: &mut dyn Pipeline| {
                pipeline
                    .write(b"void callback bytes")
                    .map_err(Error::from)?;
                pipeline.finish().map_err(Error::from)
            },
        };
        void_provider
            .provide_stream_data(ObjectRef::new(41, 2), &mut void_sink)
            .expect("void callback provider");
        assert_eq!(
            void_sink.take_buffer().expect("void callback output"),
            b"void callback bytes"
        );

        let mut retry_sink = crate::pipeline::buffer::Buffer::new("retry callback", None);
        let retry_provider = RetryCallbackProvider {
            callback: |pipeline: &mut dyn Pipeline, suppress_warnings: bool, will_retry: bool| {
                assert!(suppress_warnings);
                assert!(!will_retry);
                pipeline
                    .write(b"retry callback bytes")
                    .map_err(Error::from)?;
                pipeline.finish().map_err(Error::from)?;
                Ok(true)
            },
        };
        assert!(retry_provider.supports_retry());
        assert!(retry_provider
            .provide_stream_data_with_retry(ObjectRef::new(43, 1), &mut retry_sink, true, false,)
            .expect("retry callback provider"));
        assert_eq!(
            retry_sink.take_buffer().expect("retry callback output"),
            b"retry callback bytes"
        );
    }

    #[test]
    fn callback_adapters_are_deferred_and_retry_capability_is_retained() {
        let stream = provider_stream();
        let void_calls = Rc::new(Cell::new(0));
        let void_calls_for_callback = Rc::clone(&void_calls);
        stream
            .replace_stream_data_with_callback(
                move |_pipeline| {
                    void_calls_for_callback.set(void_calls_for_callback.get() + 1);
                    Ok(())
                },
                None,
                None,
            )
            .expect("void callback replacement");
        assert_eq!(void_calls.get(), 0, "callback registration must be lazy");
        let mut void_sink = crate::pipeline::buffer::Buffer::new("void callback", None);
        stream
            .with_value(registered_provider)
            .provide_stream_data(ObjectRef::new(41, 2), &mut void_sink)
            .expect("void callback provider");
        assert_eq!(void_calls.get(), 1, "void callback runs only when piped");

        let retry_calls = Rc::new(Cell::new(0));
        let retry_calls_for_callback = Rc::clone(&retry_calls);
        stream
            .replace_stream_data_with_retry_callback(
                move |_pipeline, _suppress_warnings, _will_retry| {
                    retry_calls_for_callback.set(retry_calls_for_callback.get() + 1);
                    Ok(true)
                },
                None,
                None,
            )
            .expect("retry callback replacement");
        assert_eq!(
            retry_calls.get(),
            0,
            "retry callback registration must be lazy"
        );
        let mut retry_sink = crate::pipeline::buffer::Buffer::new("retry callback", None);
        assert!(stream
            .with_value(registered_provider)
            .provide_stream_data_with_retry(ObjectRef::new(43, 1), &mut retry_sink, true, false)
            .expect("retry callback provider"));
        assert_eq!(retry_calls.get(), 1, "retry callback runs only when piped");

        fn is_retry_provider(value: Option<&ObjectValue>) -> bool {
            match value {
                Some(ObjectValue::Stream {
                    stream_provider: Some(provider),
                    ..
                }) => provider.supports_retry(),
                _ => false,
            }
        }

        assert!(stream.with_value(is_retry_provider));
        assert!(!ObjectHandle::integer(0).with_value(is_retry_provider));
    }
}

#[cfg(test)]
pub(crate) mod warning_emission_tests {
    use super::*;

    /// A document that records every warning an object emits through it.
    pub(crate) struct WarningRecorder {
        value: ObjectValue,
        warnings: RefCell<Vec<String>>,
    }

    impl DocumentResolver for WarningRecorder {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            handle.set_resolved(self.value.clone());
            Ok(())
        }

        fn warn(&self, message: String) -> crate::Result<()> {
            self.warnings.borrow_mut().push(message);
            Ok(())
        }
    }

    /// An indirect handle that resolves to `value`, paired with the document
    /// it warns through.
    pub(crate) fn handle_resolving(value: ObjectValue) -> (ObjectHandle, Rc<WarningRecorder>) {
        let recorder = Rc::new(WarningRecorder {
            value,
            warnings: RefCell::new(Vec::new()),
        });
        // The erased `Rc` shares its allocation with `recorder`, so the
        // returned recorder is what keeps the handle's `Weak` upgradable.
        let erased: Rc<dyn DocumentResolver> = recorder.clone();
        let handle =
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(3, 0), Rc::downgrade(&erased));
        (handle, recorder)
    }

    pub(crate) fn warnings(recorder: &Rc<WarningRecorder>) -> Vec<String> {
        recorder.warnings.borrow().clone()
    }

    #[test]
    fn type_warning_through_a_context_matches_qpdf_message_text() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        handle
            .type_warning("dictionary", "treating as empty")
            .unwrap();

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for dictionary attempted on object of type integer: treating as empty"]
        );
    }

    #[test]
    fn type_warning_names_the_type_it_actually_found() {
        let (handle, recorder) = handle_resolving(ObjectValue::Name(b"Foo".to_vec()));

        handle.type_warning("integer", "returning 0").unwrap();

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for integer attempted on object of type name: returning 0"]
        );
    }

    #[test]
    fn type_warning_without_a_context_returns_the_error_qpdf_throws() {
        let handle = ObjectHandle::integer(7);

        let error = handle
            .type_warning("dictionary", "treating as empty")
            .unwrap_err();

        assert!(matches!(
            error,
            crate::Error::System(ref message)
                if message
                    == "operation for dictionary attempted on object of type integer: \
                        treating as empty"
        ));
    }

    #[test]
    fn array_mutators_emit_qpdf_warning_text_for_invalid_domains() {
        let (array, recorder) =
            handle_resolving(ObjectValue::Array(vec![ObjectHandle::integer(1)]));

        array.set_array_item(1, ObjectHandle::integer(2)).unwrap();
        array
            .insert_array_item(2, ObjectHandle::integer(3))
            .unwrap();
        let erased = array.erase_array_item_and_get_old(1).unwrap();

        assert!(erased.is_null());
        assert_eq!(
            warnings(&recorder),
            [
                "object 3 0: ignoring attempt to set out of bounds array item",
                "object 3 0: ignoring attempt to insert out of bounds array item",
                "object 3 0: ignoring attempt to erase out of bounds array item",
            ]
        );
    }

    #[test]
    fn array_mutators_emit_type_warning_text_for_non_arrays() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        handle.set_array_item(0, ObjectHandle::integer(1)).unwrap();
        handle
            .set_array_items(vec![ObjectHandle::integer(2)])
            .unwrap();
        handle
            .insert_array_item(0, ObjectHandle::integer(3))
            .unwrap();
        handle.append_array_item(ObjectHandle::integer(4)).unwrap();
        let erased = handle.erase_array_item_and_get_old(0).unwrap();

        assert!(erased.is_null());
        assert_eq!(warnings(&recorder), [
            "object 3 0: operation for array attempted on object of type integer: ignoring attempt to set item",
            "object 3 0: operation for array attempted on object of type integer: ignoring attempt to replace items",
            "object 3 0: operation for array attempted on object of type integer: ignoring attempt to insert item",
            "object 3 0: operation for array attempted on object of type integer: ignoring attempt to append item",
            "object 3 0: operation for array attempted on object of type integer: ignoring attempt to erase item",
        ]);
    }

    /// A sink that appends every write to a shared buffer.
    struct ErrorRecordingSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl crate::pipeline::Pipeline for ErrorRecordingSink {
        // cov:ignore-start: the default logger does not inspect a sink identifier
        fn identifier(&self) -> &str {
            "error recording sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: the default logger leaves caller-owned sinks unfinished
        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    /// Serializes the tests that redirect the process-global default logger.
    ///
    /// `QPDFLogger::default_logger` is one shared instance, so two tests
    /// swapping its error sink concurrently restore each other's sink and one
    /// of them captures nothing.
    static DEFAULT_LOGGER_ERROR_SINK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `body` with the default logger's error stream captured, restoring
    /// the previous sink afterwards. Returns what `body` returned alongside
    /// the captured bytes as UTF-8.
    fn with_captured_default_error<T>(body: impl FnOnce() -> T) -> (T, String) {
        let guard = DEFAULT_LOGGER_ERROR_SINK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let logger = crate::QPDFLogger::default_logger();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let restore = logger.get_error().unwrap();
        logger.set_error(Some(crate::pipeline::PipelineHandle::new(
            ErrorRecordingSink(std::sync::Arc::clone(&captured)),
        )));

        let result = body();

        logger.set_error(Some(restore));
        drop(guard);
        let captured = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        (result, captured)
    }

    #[test]
    fn as_dictionary_on_a_non_dictionary_stays_silent_like_qpdf() {
        // `asDictionary()` is the silent internal helper; only `getKey`,
        // `getKeys` and `getDictAsMap` warn.
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        assert!(handle.try_as_dictionary().unwrap().is_none());

        assert!(warnings(&recorder).is_empty());
    }

    #[test]
    fn dictionary_accessors_warn_on_a_non_dictionary_receiver() {
        // qpdf raises `typeWarning("dictionary", "returning null for
        // attempted key retrieval")` here (`libqpdf/QPDFObjectHandle.cc:984`)
        // and its receiver always has a context, because `QPDFParser` stamps
        // the owning document on every value it creates
        // (`libqpdf/QPDFParser.cc:416-442`). Both accessors now call
        // `type_warning`, while the result shapes remain qpdf's null/empty
        // fallbacks.
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        assert!(handle.try_get_key(b"/Type").unwrap().is_null());
        assert!(handle.try_get_keys().unwrap().is_empty());

        assert_eq!(
            warnings(&recorder),
            [
                "object 3 0: operation for dictionary attempted on object of type integer: returning null for attempted key retrieval",
                "object 3 0: operation for dictionary attempted on object of type integer: treating as empty",
            ]
        );
    }

    #[test]
    fn dictionary_accessors_neither_warn_nor_change_their_result() {
        let (handle, recorder) = handle_resolving(ObjectValue::Dictionary(
            [
                (b"A".to_vec(), ObjectHandle::integer(1)),
                (b"B".to_vec(), ObjectHandle::null()),
            ]
            .into_iter()
            .collect(),
        ));

        assert_eq!(
            handle.try_get_keys().unwrap(),
            [b"/A".to_vec()].into_iter().collect::<BTreeSet<_>>()
        );
        assert_eq!(handle.try_get_key(b"/A").unwrap().as_integer(), Some(1));
        assert!(handle.try_get_key(b"/Missing").unwrap().is_null());
        assert!(warnings(&recorder).is_empty());
    }

    #[test]
    fn get_int_value_on_a_non_integer_warns_and_returns_zero() {
        // `libqpdf/QPDFObjectHandle.cc:503-513`
        let (handle, recorder) = handle_resolving(ObjectValue::Name(b"Foo".to_vec()));

        assert_eq!(handle.try_get_int_value().unwrap(), 0);

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for integer attempted on object of type name: returning 0"]
        );
    }

    #[test]
    fn an_integer_below_int_min_saturates_and_warns() {
        // `libqpdf/QPDFObjectHandle.cc:528-532`
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(i64::from(i32::MIN) - 1));

        assert_eq!(handle.try_get_int_value_as_int().unwrap(), i32::MIN);

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: requested value of integer is too small; returning INT_MIN"]
        );
    }

    #[test]
    fn an_integer_above_int_max_saturates_and_warns() {
        // `libqpdf/QPDFObjectHandle.cc:532-536`
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(i64::from(i32::MAX) + 1));

        assert_eq!(handle.try_get_int_value_as_int().unwrap(), i32::MAX);

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: requested value of integer is too big; returning INT_MAX"]
        );
    }

    #[test]
    fn the_int_endpoints_themselves_neither_saturate_nor_warn() {
        // qpdf compares strictly (`v < INT_MIN`, `v > INT_MAX`), so the
        // endpoints pass through unwarned.
        for value in [i32::MIN, -1, 0, 7, i32::MAX] {
            let (handle, recorder) = handle_resolving(ObjectValue::Integer(i64::from(value)));

            assert_eq!(handle.try_get_int_value_as_int().unwrap(), value);
            assert_eq!(handle.try_get_int_value().unwrap(), i64::from(value));
            assert!(warnings(&recorder).is_empty(), "{value} warned");
        }
    }

    #[test]
    fn a_clamp_warnings_sink_failure_propagates_instead_of_the_saturated_value() {
        // warn_if_possible's context branch calls through to the resolver's
        // own `warn`, which can fail — the trait default does, exactly the
        // case `a_resolver_without_a_warning_sink_reports_rather_than_swallows`
        // pins for type_warning. try_get_int_value_as_int must not swallow
        // that failure and hand back a saturated value as if nothing warned.
        let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(3, 0),
            Rc::downgrade(&resolver),
        );
        handle.set_resolved(ObjectValue::Integer(i64::from(i32::MAX) + 1));

        let error = handle.try_get_int_value_as_int().unwrap_err();

        assert!(matches!(
            error,
            crate::Error::Internal(ref message)
                if message.contains("returning INT_MAX")
        ));
    }

    #[test]
    fn a_non_integer_reached_as_an_int_warns_once_for_the_type_and_not_for_range() {
        // getIntValueAsInt delegates to getIntValue, whose 0 fallback is in
        // range, so only the type warning is emitted.
        let (handle, recorder) = handle_resolving(ObjectValue::Name(b"Foo".to_vec()));

        assert_eq!(handle.try_get_int_value_as_int().unwrap(), 0);

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for integer attempted on object of type name: returning 0"]
        );
    }

    /// A document that resolves but never implements a warning sink, so the
    /// trait default decides what happens.
    struct SinklessResolver;

    impl DocumentResolver for SinklessResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            handle: &ObjectHandle,
        ) -> crate::Result<()> {
            handle.set_resolved(ObjectValue::Integer(7));
            Ok(())
        }
    }

    /// A document whose resolution always fails, so a warning path can be
    /// asked whether it propagates that or misreports it as no context.
    struct FailingResolver;

    impl DocumentResolver for FailingResolver {
        fn resolve_indirect(
            &self,
            _object_ref: ObjectRef,
            _handle: &ObjectHandle,
        ) -> crate::Result<()> {
            Err(crate::Error::System("resolver failed".to_owned()))
        }
    }

    #[test]
    fn warn_if_possible_propagates_a_live_documents_resolution_failure() {
        // A reachable document that cannot resolve is not qpdf's null
        // context, so this must not silently divert to the default logger.
        let resolver: Rc<dyn DocumentResolver> = Rc::new(FailingResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(3, 0),
            Rc::downgrade(&resolver),
        );

        let error = handle.warn_if_possible("damage").unwrap_err();

        assert!(matches!(
            error,
            crate::Error::System(ref message) if message == "resolver failed"
        ));
    }

    #[test]
    fn a_stream_receiver_warns_under_its_own_type_name() {
        // `asDictionary()` is null for a stream just as it is for a scalar
        // (`libqpdf/QPDFObjectHandle.cc:999-1003`), so the dictionary
        // accessors reach this warning with `stream` as the actual type.
        let (handle, recorder) = handle_resolving(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"Length".to_vec(),
                ObjectHandle::integer(0),
            )]),
            stream_data: None,
            stream_provider: None,
            stream_length: 0,
        });

        handle
            .type_warning("dictionary", "treating as empty")
            .unwrap();

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for dictionary attempted on object of type stream: treating as empty"]
        );
    }

    #[test]
    fn a_resolver_without_a_warning_sink_reports_rather_than_swallows() {
        let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
        let handle = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(3, 0),
            Rc::downgrade(&resolver),
        );

        let error = handle
            .type_warning("dictionary", "treating as empty")
            .unwrap_err();

        assert!(matches!(
            error,
            crate::Error::Internal(ref message)
                if message
                    == "warning raised through a resolver with no document warning sink: \
                        object 3 0: operation for dictionary attempted on object of type integer: \
                        treating as empty"
        ));
    }

    #[test]
    fn warn_if_possible_treats_a_dropped_document_as_no_context() {
        // `warnIfPossible`'s guard fails on a handle it cannot dereference
        // and takes the logger branch rather than reporting
        // (`libqpdf/QPDFObjectHandle.cc:2195-2200`).
        let handle = {
            let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(3, 0), Rc::downgrade(&resolver))
        };
        assert!(
            handle.try_dereference().is_err(),
            "the document is gone, so resolution fails"
        );

        let (result, captured) =
            with_captured_default_error(|| handle.warn_if_possible("dropped document warning"));

        result.unwrap();
        assert!(
            captured.contains("dropped document warning\n"),
            "default error stream captured {captured:?}"
        );
    }

    #[test]
    fn warn_if_possible_without_a_context_logs_instead_of_failing() {
        // The else-branch of `warnIfPossible` writes the bare message to
        // `QPDFLogger::defaultLogger()->getError()` and returns normally
        // (`libqpdf/QPDFObjectHandle.cc:2196-2200`).
        let handle = {
            let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
            ObjectHandle::new_indirect_with_resolver(ObjectRef::new(3, 0), Rc::downgrade(&resolver))
        };
        let (result, captured) = with_captured_default_error(|| {
            handle.warn_if_possible("requested value of integer is too big; returning INT_MAX")
        });

        result.unwrap();
        // Every document opened without an explicit logger shares this sink,
        // so assert the exact line is present rather than that it is alone.
        assert!(
            captured.contains("requested value of integer is too big; returning INT_MAX\n"),
            "default error stream captured {captured:?}"
        );
        assert!(
            !captured.contains("object 3 0:"),
            "contextless warn_if_possible must log bare warning without object description prefix"
        );
    }

    #[test]
    fn warn_if_possible_through_a_context_reaches_the_document_sink() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        handle
            .warn_if_possible("requested value of integer is too small; returning INT_MIN")
            .unwrap();

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: requested value of integer is too small; returning INT_MIN"]
        );
    }

    #[test]
    fn object_warning_passes_its_message_through_without_dereferencing() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        handle.object_warning("unresolved name object").unwrap();

        assert_eq!(warnings(&recorder), ["object 3 0: unresolved name object"]);
        assert!(
            !handle.is_resolved(),
            "objectWarning does not dereference its receiver"
        );
    }

    #[test]
    fn object_warning_without_a_context_returns_the_error_qpdf_throws() {
        let handle = ObjectHandle::integer(7);

        let error = handle.object_warning("unresolved name object").unwrap_err();

        assert!(matches!(
            error,
            crate::Error::System(ref message) if message == "unresolved name object"
        ));
    }

    #[test]
    fn two_warnings_from_one_handle_reach_the_sink_in_emission_order() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        handle
            .type_warning("dictionary", "treating as empty")
            .unwrap();
        handle.type_warning("array", "treating as empty").unwrap();

        assert_eq!(
            warnings(&recorder),
            [
                "object 3 0: operation for dictionary attempted on object of type integer: treating as empty",
                "object 3 0: operation for array attempted on object of type integer: treating as empty",
            ]
        );
    }

    #[test]
    fn object_description_template_placeholders_and_offset_shifts() {
        let dict = ObjectHandle::dictionary(vec![]);
        dict.set_description("object $OG at offset $PO".to_owned(), 100);
        // `$OG` needs indirect identity; qpdf only ever installs that
        // together with the owning document (`QPDFValue::setDefaultDescription`,
        // `libqpdf/qpdf/QPDFValue.hh:66-71`), so promote through the same
        // atomic primitive flpdf already uses for that pairing rather than
        // writing `object_ref` alone.
        let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
        dict.promote_to_indirect(ObjectRef::new(5, 0), 1, Rc::downgrade(&resolver));
        assert_eq!(dict.description(), "object 5 0 at offset 102");

        let arr = ObjectHandle::array(vec![]);
        arr.set_description("array at offset $PO".to_owned(), 200);
        assert_eq!(arr.description(), "array at offset 201");

        let scalar = ObjectHandle::integer(42);
        scalar.set_description("scalar at offset $PO".to_owned(), 300);
        assert_eq!(scalar.description(), "scalar at offset 300");
    }

    #[test]
    fn object_description_template_preserves_partial_and_unknown_markers() {
        let cases = [
            ("$$", "$$"),
            ("$PX", "$PX"),
            ("$P", "$P"),
            ("$OX", "$OX"),
            ("$O", "$O"),
            ("$X", "$X"),
            ("trailing$", "trailing$"),
        ];

        for (template, expected) in cases {
            let handle = ObjectHandle::integer(7);
            handle.set_description(template.to_owned(), 300);
            assert_eq!(handle.description(), expected, "template {template:?}");
        }
    }

    #[test]
    fn object_description_template_replaces_each_qpdf_marker_only_once() {
        let handle = ObjectHandle::dictionary(vec![]);
        handle.set_description("$$/$PO/$PO/$OG/$OG".to_owned(), 100);
        let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
        handle.promote_to_indirect(ObjectRef::new(5, 0), 1, Rc::downgrade(&resolver));

        assert_eq!(handle.description(), "$$/102/$PO/5 0/$OG");
    }

    #[test]
    fn set_description_does_not_overwrite_an_already_recorded_parsed_offset() {
        // Mirrors a parsed stream: `ObjectSlot::parsed_offset` already holds
        // the encoded stream-data start `pipe_stream_data` reads from before
        // any description is attached. `QPDFValue::setDescription` routes
        // through the same set-once `setParsedOffset` guard every other
        // caller uses (`libqpdf/qpdf/QPDFValue.hh:60-65,90-100`), so the
        // operational offset must survive.
        let stream = ObjectHandle::integer(7);
        stream.set_parsed_offset_if_unset(500);

        stream.set_description("stream object $OG at offset $PO".to_owned(), 999);

        assert_eq!(stream.get_parsed_offset(), 500);
        assert_eq!(stream.description(), "stream object  at offset 500");
    }

    #[test]
    fn set_description_json_does_not_overwrite_an_already_recorded_parsed_offset() {
        let stream = ObjectHandle::integer(7);
        stream.set_parsed_offset_if_unset(500);

        stream.set_description_json("input.pdf".to_owned(), "object 1 0".to_owned(), 999);

        assert_eq!(stream.get_parsed_offset(), 500);
        assert_eq!(stream.description(), "input.pdf, object 1 0 at offset 500");
    }

    #[test]
    fn description_template_ignores_non_template_descriptions() {
        let without_description = ObjectHandle::integer(7);
        assert_eq!(without_description.description_template(), None);

        let json = ObjectHandle::null();
        json.set_description_json("input.pdf".to_owned(), "object 1 0".to_owned(), 123);
        assert_eq!(json.description_template(), None);

        let parent = ObjectHandle::dictionary(vec![]);
        let child = ObjectHandle::null();
        child.set_child_description(&parent, " -> dictionary key $VD", "/Value");
        assert_eq!(child.description_template(), None);
    }

    #[test]
    fn object_description_child_chaining_and_var_descr() {
        let parent = ObjectHandle::dictionary(vec![]);
        parent.set_description("object 5 0 at offset 253".to_owned(), 253);

        let child = ObjectHandle::null();
        child.set_child_description(&parent, " -> dictionary key $VD", "/EF");

        assert_eq!(
            child.description(),
            "object 5 0 at offset 253 -> dictionary key /EF"
        );
    }

    #[test]
    fn object_description_child_replaces_only_the_first_qpdf_marker() {
        let parent = ObjectHandle::dictionary(vec![]);
        parent.set_description("parent $VD".to_owned(), 253);

        let child = ObjectHandle::null();
        child.set_child_description(&parent, " -> dictionary key $VD", "/EF");

        assert_eq!(child.description(), "parent /EF -> dictionary key $VD");
    }

    #[test]
    fn object_description_indirect_fallback() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(12, 0), NO_PARSED_OFFSET);
        assert_eq!(handle.description(), "object 12 0");
    }

    #[test]
    fn object_description_type_warning_includes_parent_and_object_path() {
        let (parent, recorder) =
            handle_resolving(ObjectValue::Dictionary(std::collections::BTreeMap::new()));
        parent.set_description("object 5 0 at offset 253".to_owned(), 253);

        let child = parent.try_get_key(b"/EF").unwrap();
        child
            .type_warning("dictionary", "treating as empty")
            .unwrap();

        assert_eq!(
            warnings(&recorder),
            ["object 5 0 at offset 253 -> dictionary key /EF: operation for dictionary attempted on object of type null: treating as empty"]
        );
    }

    #[test]
    fn object_description_json_and_negative_offset_and_unresolved() {
        let handle1 = ObjectHandle::null();
        handle1.set_description_json("input.pdf".to_owned(), "".to_owned(), 123);
        assert_eq!(handle1.description(), "input.pdf at offset 123");

        let handle2 = ObjectHandle::null();
        handle2.set_description_json("input.pdf".to_owned(), "object 1 0".to_owned(), 456);
        assert_eq!(handle2.description(), "input.pdf, object 1 0 at offset 456");

        let handle3 = ObjectHandle::null();
        handle3.set_description("item at offset $PO".to_owned(), -1);
        assert_eq!(handle3.description(), "item at offset -1");

        let resolver: Rc<dyn DocumentResolver> = Rc::new(SinklessResolver);
        let handle4 = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(3, 0),
            Rc::downgrade(&resolver),
        );
        handle4.set_description("unresolved at offset $PO".to_owned(), 50);
        assert_eq!(handle4.description(), "unresolved at offset 50");
    }

    #[test]
    fn object_context_traverses_containment_parents() {
        let (parent, recorder) = handle_resolving(ObjectValue::Array(vec![]));
        let child = ObjectHandle::integer(10);
        ObjectHandle::attach_child_to_parent(&child, &Rc::downgrade(&parent.0));

        child
            .type_warning("dictionary", "treating as empty")
            .unwrap();
        assert_eq!(
            warnings(&recorder),
            ["operation for dictionary attempted on object of type integer: treating as empty"]
        );
    }

    #[test]
    fn dictionary_accessors_warn_through_a_direct_child_context() {
        let (parent, recorder) = handle_resolving(ObjectValue::Array(vec![]));
        let child = ObjectHandle::integer(10);
        ObjectHandle::attach_child_to_parent(&child, &Rc::downgrade(&parent.0));

        assert!(child.try_get_keys().unwrap().is_empty());
        assert_eq!(
            warnings(&recorder),
            ["operation for dictionary attempted on object of type integer: treating as empty"]
        );
    }

    #[test]
    fn warn_if_possible_through_a_context_with_no_description_omits_the_prefix() {
        // A live context found only via containment (no description of its
        // own) must still emit the bare warning, matching `desc.is_empty()`
        // in `warnIfPossible`'s own branch (`libqpdf/QPDFObjectHandle.cc:2196-2199`).
        let (parent, recorder) = handle_resolving(ObjectValue::Array(vec![]));
        let child = ObjectHandle::integer(10);
        ObjectHandle::attach_child_to_parent(&child, &Rc::downgrade(&parent.0));

        child.warn_if_possible("treating as empty").unwrap();

        assert_eq!(warnings(&recorder), ["treating as empty"]);
    }

    #[test]
    fn try_get_key_without_leading_slash_formats_key() {
        let parent = ObjectHandle::dictionary(vec![]);
        let child = parent.try_get_key(b"/NoSlash").unwrap();
        assert_eq!(child.description(), " -> dictionary key /NoSlash");
    }

    #[test]
    fn try_get_key_with_leading_slash_formats_key() {
        let parent = ObjectHandle::dictionary(vec![]);
        let child = parent.try_get_key(b"/EF").unwrap();
        assert_eq!(child.description(), " -> dictionary key /EF");
    }
}
