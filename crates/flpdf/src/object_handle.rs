//! The core object-handle graph: shared, cloneable identity for direct and
//! indirect PDF objects, with qpdf-compatible parsed-offset tracking and the
//! document-owned reserved construction sentinel.
//!
//! qpdf correspondence: `QPDFObjectHandle`, `QPDFObject`, and `QPDFValue` identity and payload ownership, `QPDF::newReserved`/`QPDF_Reserved`, `QPDFObjectHandle::copyStream`/`QPDF::copyStreamData` stream-copy primitives, and `QPDF::setImmediateCopyFrom`.
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
//!
//! qpdf stores a stream's deferred source as a
//! `std::shared_ptr<QPDFObjectHandle::StreamDataProvider>` in
//! `QPDF_Stream` (`include/qpdf/QPDFObjectHandle.hh:68-127`,
//! `libqpdf/QPDF_Stream.cc:575-604,640-660`). [`ObjectValue::Stream`] uses
//! `Rc<dyn StreamDataProvider>` as an internal container substitution: it
//! preserves provider ownership, exclusive buffer/provider source state,
//! lazy invocation, repeated-call behavior, and the source pipeline order.
//! The `Rc` choice is an internal Rust ownership detail; qpdf's observable
//! callback, identity, retry, error, and `/Length` contracts remain the
//! authority.
//
// qpdf-deviation-start: qpdf 11.9.0's default destruction of a sufficiently
// deep programmatic direct container graph recursively follows the
// shared-pointer children. Its depth-unbounded direct factories and default
// container destructors are documented by `QPDFObjectHandle.cc:1944-2013`,
// `QPDF_Array.hh:9-50`, and `QPDF_Dictionary.hh:11-38`; a pinned live probe
// drops depth 5,000 successfully but finishes construction and exits 139 at
// depths 50,000 and 100,000. flpdf automatically drains only final-owner
// acyclic direct children with a heap worklist, preserving shared identity,
// payload, and PDF output while avoiding Rust stack exhaustion.
// qpdf-deviation-end
//!
//! ## Page/Form content-family correspondence
//!
//! The content-family surface below is a direct qpdf 11.9.0 responsibility
//! mapping. The qpdf declarations and implementation ranges are kept beside
//! the Rust entry points so every consumer can be checked against one
//! canonical route rather than adding a second parser or decoder.
//!
//! | qpdf 11.9.0 declaration/implementation | flpdf entry point | regression evidence |
//! | --- | --- | --- |
//! | `QPDFObjectHandle::TokenFilter`, `QPDFObjectHandle::ParserCallbacks` (`include/qpdf/QPDFObjectHandle.hh:129-227`) | [`crate::token_filter::TokenFilter`], [`crate::content_stream::ObjectHandleParserCallbacks`], [`crate::content_stream::ParseControl`] | `object_handle_content_parser_tests.rs`: token-filter output/discard/EOF and parser callback lifecycle tests |
//! | `parseContentStream`, `pipeContentStreams`, `addTokenFilter`, `parsePageContents`, `filterPageContents`, `pipePageContents`, `addContentTokenFilter`, `filterAsContents`, `parseAsContents` (`include/qpdf/QPDFObjectHandle.hh:421-473`) | [`ObjectHandle::parse_page_contents`], [`ObjectHandle::parse_as_contents`], [`ObjectHandle::filter_page_contents`], [`ObjectHandle::filter_as_contents`], [`ObjectHandle::pipe_page_contents`], [`ObjectHandle::pipe_content_streams`], [`ObjectHandle::add_token_filter`], [`ObjectHandle::add_content_token_filter`], `ObjectHandle::parse_content_stream_handles` (private orchestration for `parseContentStream`) | `object_handle_content_parser_tests.rs` and `object_handle_page_content_pipeline_tests.rs` |
//! | `makeResourcesIndirect` (`include/qpdf/QPDFObjectHandle.hh:789-793`; `libqpdf/QPDFObjectHandle.cc:1042-1060`) | [`ObjectHandle::make_resources_indirect`] | `object_handle::mutation_tests::make_resources_indirect_promotes_direct_second_level_values_only` and `acroform_document_helper::tests::prepare_foreign_resource_plan_indirectizes_both_dr_second_level_values` |
//! | `getResourceNames` (`include/qpdf/QPDFObjectHandle.hh:831-835`; `libqpdf/QPDFObjectHandle.cc:1156-1170`) | [`ObjectHandle::get_resource_names`] | `public_object_primitives.rs::qpdf_object_handle_primitives_are_available_to_external_crates` |
//! | `getUniqueResourceName` (`include/qpdf/QPDFObjectHandle.hh:837-850`) | [`ObjectHandle::get_unique_resource_name`] | `object_handle_content_shape_tests.rs::unique_resource_name_uses_the_supplied_prefix_and_suffix_cursor` and related shape tests |
//! | `getPageContents`, `addPageContents`, `rotatePage`, `coalesceContentStreams` (`include/qpdf/QPDFObjectHandle.hh:1242-1254`) | [`ObjectHandle::get_page_contents`], [`ObjectHandle::add_page_contents`], [`ObjectHandle::rotate_page`], [`ObjectHandle::coalesce_content_streams`] | `object_handle_content_shape_tests.rs` and `object_handle_page_content_pipeline_tests.rs` |
//! | `isFormXObject`, `isImage` (`include/qpdf/QPDFObjectHandle.hh:1328-1334`) | [`ObjectHandle::is_form_xobject`], [`ObjectHandle::is_image`] | `object_handle_content_shape_tests.rs::form_and_image_classification_matches_qpdf` |
//! | `CoalesceProvider` (`libqpdf/QPDFObjectHandle.cc:94-118`) | `CoalesceContentProvider` and [`ObjectHandle::coalesce_content_streams`] | `object_handle_page_content_pipeline_tests.rs::coalesce_content_streams_installs_a_lazy_document_owned_provider` |
//! | `arrayOrStreamToStreamArray`, `getPageContents`, `addPageContents`, `rotatePage`, `coalesceContentStreams` (`libqpdf/QPDFObjectHandle.cc:1438-1572`) | `array_or_stream_to_stream_array`, [`ObjectHandle::get_page_contents`], [`ObjectHandle::add_page_contents`], [`ObjectHandle::rotate_page`], [`ObjectHandle::coalesce_content_streams`] | shape normalization, prepend/append, inherited rotation, malformed-array, and lazy-provider tests |
//! | `pipePageContents`, `pipeContentStreams` (`libqpdf/QPDFObjectHandle.cc:1702-1737`) | [`ObjectHandle::pipe_page_contents`], [`ObjectHandle::pipe_content_streams`] | `object_handle_page_content_pipeline_tests.rs::pipe_page_contents_decodes_and_joins_streams_with_qpdf_newline_rules` and failure/description tests |
//! | `parsePageContents`, `parseAsContents`, `filterPageContents`, `filterAsContents`, `parseContentStream_internal`, inline-image recovery, `addContentTokenFilter`, `addTokenFilter` (`libqpdf/QPDFObjectHandle.cc:1740-1859`) | [`ObjectHandle::parse_page_contents`], [`ObjectHandle::parse_as_contents`], [`ObjectHandle::filter_page_contents`], [`ObjectHandle::filter_as_contents`], [`ObjectHandle::add_content_token_filter`], [`ObjectHandle::add_token_filter`], [`crate::content_stream::parse_content_stream_handles`] | `object_handle_content_parser_tests.rs` callback identity/span/diagnostic/early-stop/inline-image/filter tests |
//! | `isFormXObject`, `isImage` (`libqpdf/QPDFObjectHandle.cc:2340-2352`) | [`ObjectHandle::is_form_xobject`], [`ObjectHandle::is_image`] | direct/indirect subtype and ImageMask exclusion tests |
//!
//! `QPDFParser::parseRemainder` and the tokenizer dispatch that backs the
//! ObjectHandle callbacks remain in [`crate::parser`] and
//! `crate::pipeline::qpdf_tokenizer::QpdfTokenizer`; their qpdf source
//! correspondence is documented at
//! `libqpdf/QPDFParser.cc:135-176,221-223,266-274,408-444,456-469` and
//! `libqpdf/Pl_QPDFTokenizer.cc:36-65`. This module owns the handle identity,
//! stream normalization, provider/filter dispatch, and public consumer
//! boundary; PageObjectHelper and page-tree orchestration are intentionally
//! outside this table and tracked separately.

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
// valid array access. It emits no bytes or diagnostics; the warning-producing
// signed-index surface is try_get_array_item and the try_*_array_item_at
// mutators. See docs/qpdf-correspondence.md.

use crate::matrix::Rectangle;
use crate::pdf_string::{decode_pdf_text_string, lossy_utf16_to_utf8, utf8_value};
use crate::token_filter::TokenFilter;
use crate::{
    content_normalizer::ContentNormalizerPipeline,
    content_stream::{parse_content_stream_handles, ObjectHandleParserCallbacks},
    pipeline::{
        buffer::Buffer,
        count::Count,
        flate::{Flate, FlateAction, DEFAULT_OUT_BUFFER_SIZE},
        qpdf_tokenizer::QpdfTokenizer,
        Discard, Pipeline, PipelineError, PipelineRef, PlString,
    },
    stream_filter::{
        decode_params_from_handle, normalize_filter_name, stream_filter_for, OwnedDecodePipeline,
        StreamFilter, DECODE_PARMS_LENGTH_ERROR, FILTER_TYPE_ERROR,
    },
    writer::DecodeLevel,
};
use crate::{json::Json, Error, ObjectRef, Result};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::rc::{Rc, Weak};

/// Maximum inline structural nesting depth used by canonical graph walkers.
pub(crate) const MAX_INLINE_DEPTH: usize = 256;

type StreamTokenFilter = Rc<RefCell<dyn TokenFilter>>;
type StreamTokenFilterList = Rc<RefCell<Vec<StreamTokenFilter>>>;

/// qpdf's `qpdf_ef_compress` bit in `QPDF_Stream::pipeStreamData`, for the
/// `encode_flags` argument of [`ObjectHandle::pipe_stream_data`].
pub const STREAM_ENCODE_COMPRESS: u32 = 1;

/// qpdf's `qpdf_ef_normalize` bit in `QPDF_Stream::pipeStreamData`, for the
/// `encode_flags` argument of [`ObjectHandle::pipe_stream_data`].
pub const STREAM_ENCODE_NORMALIZE: u32 = 2;

const STREAM_DATA_PROVIDER_DEFAULT_ERROR: &str =
    "you must override provideStreamData -- see QPDFObjectHandle.hh";
const STREAM_DATA_PROVIDER_REQUIRES_INDIRECT_ERROR: &str =
    "stream data provider requires an indirect stream";
const FOREIGN_OBJECT_OWNERSHIP_ERROR: &str =
    "Attempting to add an object from a different QPDF. Use QPDF::copyForeignObject to add objects from another file.";

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

/// qpdf's `CoalesceProvider` (`QPDFObjectHandle.cc:94-118`), retaining the
/// page and the pre-coalesced `/Contents` handle so the replacement stream
/// stays lazy and reuses the canonical content-stream pipeline every time it
/// is read.
struct CoalesceContentProvider {
    containing_page: ObjectHandle,
    old_contents: ObjectHandle,
}

impl StreamDataProvider for CoalesceContentProvider {
    fn provide_stream_data_by_id(
        &self,
        _object_number: u32,
        _generation: u16,
        pipeline: &mut dyn Pipeline,
    ) -> Result<()> {
        let description = format!(
            "page object {}",
            object_generation_description(&self.containing_page)
        );
        let mut all_description = String::new();
        self.old_contents
            .pipe_content_streams(pipeline, &description, &mut all_description)
    }
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

    /// The owning document identity carried by qpdf's `QPDF*` on parser-made
    /// direct values (`QPDFValue::setDescription`,
    /// `libqpdf/qpdf/QPDFValue.hh:60-66`). Detached/test resolvers have no
    /// document owner.
    fn pdf_unique_id(&self) -> Option<u64> {
        None
    }

    /// The input name carried by qpdf's `InputSource`, used when a stream
    /// accessor constructs a `QPDFExc` at the document boundary.
    fn input_description(&self) -> String {
        String::new()
    }

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

    /// Create qpdf's lazy provider for an original file-backed foreign
    /// stream. Unlike a provider-backed source, this provider must not retain
    /// the source `ObjectHandle`: qpdf captures the source input and stream
    /// metadata so the source `QPDF` may be destroyed after the copy.
    fn original_stream_data_provider(
        &self,
        source: &ObjectHandle,
        destination_dict: &ObjectHandle,
    ) -> Result<Rc<dyn StreamDataProvider>> {
        let _ = (source, destination_dict);
        Err(Error::Internal(
            "original stream data provider requested from a resolver without a document".to_owned(),
        ))
    }

    /// The destination-aware form used by qpdf's foreign-stream dispatch.
    /// Test-only resolver implementations can keep the default behavior;
    /// document-backed resolvers override it to route deferred warnings to
    /// the destination document.
    fn original_stream_data_provider_for_destination(
        &self,
        source: &ObjectHandle,
        destination_dict: &ObjectHandle,
        destination_resolver: Weak<dyn DocumentResolver>,
    ) -> Result<Rc<dyn StreamDataProvider>> {
        let _ = destination_resolver;
        self.original_stream_data_provider(source, destination_dict)
    }

    /// Deliver a warning raised while a destination stream is reading a
    /// foreign source. qpdf's `pipeForeignStreamData` passes the destination
    /// `QPDF` as `qpdf_for_warning` (`libqpdf/QPDF.cc:2565-2585`), even though
    /// the bytes, `last_offset`, and location text (the exception's filename)
    /// all belong to the captured source input: the static `pipeStreamData`
    /// builds its `QPDFExc` from the explicit `file` argument, not from
    /// `qpdf_for_warning` (`libqpdf/QPDF.cc:2477-2530`;
    /// `libqpdf/QPDF_encryption.cc:1122-1128`). `description_override` carries
    /// that captured source description when set; `self`'s own description is
    /// used only for the ordinary (non-foreign) caller, which passes `None`.
    fn warn_stream_data(
        &self,
        _offset: u64,
        description_override: Option<&str>,
        message: String,
    ) -> Result<()> {
        let _ = (description_override, message);
        Err(Error::Internal(
            "stream data warning requested from a resolver without a document".to_owned(),
        ))
    }

    /// Whether this resolver is a qpdf source configured for immediate stream
    /// copying (`QPDF::setImmediateCopyFrom`).
    fn immediate_copy_from(&self) -> bool {
        false
    }

    /// Whether this document's writer is currently emitting its PCLm stream
    /// queue (`QPDFWriter::willFilterStream` -> `QPDF::pipeStreamData`,
    /// `libqpdf/QPDFWriter.cc:2068-2098,2928-3005`). Checked at the
    /// *destination* resolver so a foreign, provider-backed stream copied
    /// into a PCLm document also selects qpdf's full recovered-length
    /// boundary, matching the local (non-foreign) stream path.
    fn pclm_mode(&self) -> bool {
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
/// Document-created handles resolve indirect references lazily through their
/// document's qpdf-compatible resolver.
#[derive(Clone)]
pub struct ObjectHandle(Rc<RefCell<ObjectSlot>>);

impl Default for ObjectHandle {
    fn default() -> Self {
        Self::uninitialized()
    }
}

/// Opaque canonical identity for an [`ObjectHandle`].
///
/// The retained slot keeps the allocation alive while a traversal set uses
/// the key. Equality and hashing compare the slot pointer only, matching
/// qpdf's `QPDFObjectHandle::isSameObjectAs` rather than structural value
/// equality (`include/qpdf/QPDFObjectHandle.hh:304-309`,
/// `libqpdf/QPDFObjectHandle.cc:224-227`).
#[derive(Clone)]
pub(crate) struct ObjectHandleIdentity(Rc<RefCell<ObjectSlot>>);

impl PartialEq for ObjectHandleIdentity {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ObjectHandleIdentity {}

impl Hash for ObjectHandleIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Rc::as_ptr(&self.0).hash(state);
    }
}

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
            ObjectValue::Unresolved => "Unresolved",
            ObjectValue::Reserved => "Reserved",
            ObjectValue::Destroyed => "Destroyed",
            _ => "Resolved(..)",
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
pub(crate) struct JsonDescription {
    pub(crate) input: String,
    pub(crate) object: String,
}

#[derive(Clone)]
pub(crate) enum ObjectDescription {
    Template(String),
    Json(JsonDescription),
    Child(ChildDescription),
}

fn expand_description_template(
    template: &str,
    object_ref: Option<ObjectRef>,
    state: &ObjectValue,
    parsed_offset: i64,
) -> String {
    let og = object_ref
        .map(|object_ref| format!("{} {}", object_ref.number, object_ref.generation))
        .unwrap_or_default();
    let shift = match state {
        ObjectValue::Dictionary(_) | ObjectValue::Stream { .. } => 2,
        ObjectValue::Array(_) => 1,
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

/// Format the `QPDFExc::createWhat` boundary used by qpdf stream accessors.
///
/// qpdf's `createWhat` (`libqpdf/QPDFExc.cc:16-48`) only skips the
/// parenthesized `(object, offset)` segment when `object` is empty AND
/// `offset` is exactly zero. A negative offset with an empty object still
/// enters that branch and emits an empty `()` — qpdf's own literal behavior,
/// not a special case flpdf adds. Only a strictly positive offset ever
/// contributes "offset N" text inside the parentheses.
fn format_qpdf_exception_what(filename: &str, object: &str, offset: i64, message: &str) -> String {
    let mut result = filename.to_owned();
    if !(object.is_empty() && offset == 0) {
        if !filename.is_empty() {
            result.push_str(" (");
        }
        if !object.is_empty() {
            result.push_str(object);
            if offset > 0 {
                result.push_str(", ");
            }
        }
        if offset > 0 {
            result.push_str("offset ");
            result.push_str(&offset.to_string());
        }
        if !filename.is_empty() {
            result.push(')');
        }
    }
    if !result.is_empty() {
        result.push_str(": ");
    }
    result.push_str(message);
    result
}

// Deliberately not `Debug`: see `ObjectHandle`'s own hand-written `Debug`
// impl above for why a derived one is unsafe here (object-handle cycles).
// This uniform allocation corresponds to qpdf's QPDFObject/QPDFValue pair:
// it keeps the current payload and all indirect metadata together rather
// than placing direct and indirect forms in separate backing storage.
struct ObjectSlot {
    /// Whether this handle points at an actual qpdf object allocation.
    /// qpdf's default-constructed `QPDFObjectHandle` has no value at all;
    /// keeping this separate from `ObjectValue::Unresolved` preserves that
    /// state from both an initialized lazy indirect object and a null value.
    initialized: bool,
    /// The payload state is separately reference-counted so qpdf's
    /// `QPDFObject::assign` boundary can make two distinct handles observe
    /// one replacement value while retaining their own handle identities.
    state: Rc<RefCell<ObjectValue>>,
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
    /// The document identity claimed by a handle-native name/number-tree
    /// wrapper while this shared root is still contextless. qpdf's tree
    /// helper retains its owning `QPDF` alongside the shared object handle;
    /// this token preserves that single-document boundary for flpdf's
    /// per-call `&mut Pdf` API without introducing a raw-object bridge.
    tree_pdf_unique_id: Option<u64>,
    containment_parents: Vec<Weak<RefCell<ObjectSlot>>>,
    description: Option<ObjectDescription>,
    /// qpdf's `QPDF_Stream::token_filters` list. It is attached to the
    /// canonical handle allocation rather than eagerly rewriting the source
    /// bytes; the stream pipeline consumes it after decoding and before
    /// normalization/encoding (`libqpdf/QPDF_Stream.cc:488-620`).
    stream_token_filters: StreamTokenFilterList,
    /// Whether the current stream bytes have already passed the CLI's
    /// content-normalization pass. This is deliberately separate from
    /// `replaceStreamData`: qpdf treats replacement bytes as ordinary stream
    /// data, while the writer must skip only the normalization that this
    /// particular consumer has already performed.
    content_normalization_applied: Rc<Cell<bool>>,
    /// Monotonic identity for in-place mutations of this handle's payload.
    /// Writer caches use it to distinguish a plan-time stream snapshot from
    /// later qpdf-shaped replacement/filter mutations.
    mutation_generation: Rc<Cell<u64>>,
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
/// `QPDFValue` type family (`libqpdf/qpdf/QPDFValue.hh`).
///
/// Array and dictionary children are [`ObjectHandle`]s rather than raw
/// nested `ObjectValue`s, so cloning a container clones only `Rc` handles
/// (O(1) per child), not the subtree.
#[derive(Debug, Clone)]
pub(crate) enum ObjectValue {
    Null,
    /// qpdf's lazy cache sentinel (`QPDF_Unresolved`), whose object identity
    /// and resolver context remain on the surrounding [`ObjectSlot`].
    Unresolved,
    /// qpdf's document-allocation sentinel (`QPDF_Reserved`).
    Reserved,
    /// qpdf's post-document-lifetime sentinel (`QPDF_Destroyed`).
    Destroyed,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    /// Preserves a non-canonical source spelling (e.g. `.4`) alongside its
    /// parsed value, so that a real number written in the source PDF unparses
    /// byte-identically.
    RealLiteral {
        value: f64,
        literal: Vec<u8>,
    },
    Name(Vec<u8>),
    String(Vec<u8>),
    /// A content-stream operator token (e.g. `q`, `Do`). Only meaningful inside a content stream
    /// (`include/qpdf/QPDFObjectHandle.hh:318-319`: "Operator and
    /// InlineImage are only allowed in content streams").
    Operator(Vec<u8>),
    /// Raw inline-image (`BI`...`ID`...`EI`) bytes. Same content-stream-only constraint
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
        /// Whether qpdf's writer may decode, normalize, or recompress this
        /// stream. qpdf's `QPDF_Stream` initializes this to true and keeps it
        /// as stream state rather than as a serialized dictionary entry
        /// (`libqpdf/QPDF_Stream.cc:114-118,154-164`).
        filter_on_write: bool,
        /// Parse-time length for the original-source branch. qpdf's
        /// `replaceFilterData` updates `/Length` but not this member
        /// (`libqpdf/QPDF_Stream.cc:668-685`).
        stream_length: usize,
    },
}

fn empty_object_slot() -> Rc<RefCell<ObjectSlot>> {
    Rc::new(RefCell::new(ObjectSlot {
        initialized: false,
        state: Rc::new(RefCell::new(ObjectValue::Unresolved)),
        state_owners: Rc::new(RefCell::new(Vec::new())),
        object_ref: None,
        active_pdf_unique_id: None,
        resolver: None,
        parsed_offset: NO_PARSED_OFFSET,
        end_before_space: NO_PARSED_OFFSET,
        end_after_space: NO_PARSED_OFFSET,
        pdf_unique_ids: BTreeSet::new(),
        tree_pdf_unique_id: None,
        containment_parents: Vec::new(),
        description: None,
        stream_token_filters: Rc::new(RefCell::new(Vec::new())),
        content_normalization_applied: Rc::new(Cell::new(false)),
        mutation_generation: Rc::new(Cell::new(0)),
    }))
}

impl ObjectValue {
    /// Return qpdf's value-layer type ordinal.
    ///
    /// qpdf stores this as `QPDFValue::type_code` and lets
    /// `QPDFObject::getTypeCode` read it directly
    /// (`libqpdf/qpdf/QPDFObject_private.hh:42-50`). The enum match is the
    /// Rust equivalent of each `QPDFValue` subclass initializing that field.
    pub(crate) fn type_code(&self) -> u8 {
        match self {
            Self::Null => 2,
            Self::Unresolved => 13,
            Self::Reserved => 1,
            Self::Destroyed => 14,
            Self::Boolean(_) => 3,
            Self::Integer(_) => 4,
            Self::Real(_) | Self::RealLiteral { .. } => 5,
            Self::String(_) => 6,
            Self::Name(_) => 7,
            Self::Array(_) => 8,
            Self::Dictionary(_) => 9,
            Self::Stream { .. } => 10,
            Self::Operator(_) => 11,
            Self::InlineImage(_) => 12,
        }
    }

    /// Return qpdf's value-layer type name.
    ///
    /// This mirrors `QPDFValue::type_name`, which
    /// `QPDFObject::getTypeName` reads alongside `type_code`
    /// (`libqpdf/qpdf/QPDFObject_private.hh:52-60`).
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Unresolved => "unresolved",
            Self::Reserved => "reserved",
            Self::Destroyed => "destroyed",
            Self::Boolean(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Real(_) | Self::RealLiteral { .. } => "real",
            Self::String(_) => "string",
            Self::Name(_) => "name",
            Self::Array(_) => "array",
            Self::Dictionary(_) => "dictionary",
            Self::Stream { .. } => "stream",
            Self::Operator(_) => "operator",
            Self::InlineImage(_) => "inline-image",
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ContainmentOwner {
    pdf_unique_id: Option<u64>,
    object_ref: ObjectRef,
}

/// The all-zero-default matrix nested inside qpdf's `QPDFObjectHandle`.
///
/// This is intentionally distinct from [`crate::Matrix`]. qpdf's nested
/// `QPDFObjectHandle::Matrix` defaults to six zeroes, while the standalone
/// affine matrix used by flpdf defaults to the identity transform
/// (`include/qpdf/QPDFObjectHandle.hh:239-267`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ObjectHandleMatrix {
    /// Horizontal scale and rotation component.
    pub a: f64,
    /// Vertical rotation component.
    pub b: f64,
    /// Horizontal rotation component.
    pub c: f64,
    /// Vertical scale and rotation component.
    pub d: f64,
    /// Horizontal translation.
    pub e: f64,
    /// Vertical translation.
    pub f: f64,
}

impl ObjectHandleMatrix {
    /// Construct a qpdf object-handle matrix from its six components.
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }
}

/// A qpdf-shaped view over the items of an array handle.
pub struct ArrayItems {
    array: ObjectHandle,
}

/// A reversible cursor over [`ArrayItems`]. The cursor keeps the canonical
/// array handle and returns the live child at its current position.
pub struct ArrayItemCursor {
    array: ObjectHandle,
    index: usize,
    current: ObjectHandle,
}

/// A qpdf-shaped view over the entries of a dictionary handle.
pub struct DictItems {
    dictionary: ObjectHandle,
    keys: Rc<Vec<Vec<u8>>>,
}

/// One dictionary entry returned by [`DictItemCursor::current`]. At the end
/// cursor its value is an explicit uninitialized handle, matching qpdf's
/// invalidated iterator reference rather than a null object.
pub struct DictItem {
    /// The canonical slash-prefixed dictionary key.
    pub key: Vec<u8>,
    /// The live value handle, or an uninitialized handle at the end.
    pub value: ObjectHandle,
}

/// A reversible cursor over [`DictItems`].
pub struct DictItemCursor {
    dictionary: ObjectHandle,
    keys: Rc<Vec<Vec<u8>>>,
    index: usize,
    current: ObjectHandle,
}

impl ArrayItems {
    /// Return a cursor positioned at the first item, or at end for an empty
    /// array.
    pub fn begin(&self) -> ArrayItemCursor {
        ArrayItemCursor::new(self.array.clone(), 0)
    }

    /// Return a cursor positioned at the end sentinel.
    pub fn end(&self) -> ArrayItemCursor {
        ArrayItemCursor::new(self.array.clone(), self.len())
    }

    fn len(&self) -> usize {
        ArrayItemCursor::array_len(&self.array)
    }
}

impl ArrayItemCursor {
    fn new(array: ObjectHandle, index: usize) -> Self {
        let mut cursor = Self {
            array,
            index,
            current: ObjectHandle::uninitialized(),
        };
        cursor.update_current();
        cursor
    }

    /// Read the array length without cloning every child handle, unlike
    /// [`ObjectHandle::as_array`]. Every cursor step calls this, so an O(n)
    /// clone here would make a full traversal O(n²).
    fn array_len(array: &ObjectHandle) -> usize {
        array.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => children.len(),
            _ => 0,
        })
    }

    fn len(&self) -> usize {
        Self::array_len(&self.array)
    }

    fn update_current(&mut self) {
        let target = self
            .array
            .with_value(|value| match value {
                Some(ObjectValue::Array(children)) => children.get(self.index).cloned(),
                _ => None,
            })
            .filter(|item| item.is_initialized());
        self.current.rebind_cursor_value(target);
    }

    /// Return the live item at the cursor, or an uninitialized handle at end.
    /// Every clone returned by this method shares the cursor's value cell, so
    /// a prior value observes the same end/next-item transition as qpdf's
    /// iterator `ivalue` reference (`QPDFObjectHandle.cc:2488-2543`).
    pub fn current(&mut self) -> ObjectHandle {
        self.update_current();
        self.current.clone()
    }

    /// Advance one item, retaining the end sentinel when already at end.
    pub fn next(&mut self) {
        self.index = self.index.saturating_add(1).min(self.len());
        self.update_current();
    }

    /// Move one item backward, retaining the begin sentinel when already at
    /// the first item.
    pub fn previous(&mut self) {
        self.index = self.index.saturating_sub(1);
        self.update_current();
    }

    /// Whether the cursor is at or beyond the current end.
    pub fn is_end(&self) -> bool {
        self.index >= self.len()
    }
}

impl DictItems {
    /// Return a cursor positioned at the first key, or at end for an empty
    /// dictionary.
    pub fn begin(&self) -> DictItemCursor {
        DictItemCursor::new(self.dictionary.clone(), self.keys.clone(), 0)
    }

    /// Return a cursor positioned at the end sentinel.
    pub fn end(&self) -> DictItemCursor {
        DictItemCursor::new(self.dictionary.clone(), self.keys.clone(), self.len())
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

impl DictItemCursor {
    fn new(dictionary: ObjectHandle, keys: Rc<Vec<Vec<u8>>>, index: usize) -> Self {
        let mut cursor = Self {
            dictionary,
            keys,
            index,
            current: ObjectHandle::uninitialized(),
        };
        cursor.update_current();
        cursor
    }

    /// Look up the current key without cloning the whole entry map, unlike
    /// [`ObjectHandle::as_dictionary`]. Every cursor step calls this, so an
    /// O(n) clone here would make a full traversal O(n²).
    fn update_current(&mut self) {
        let target = self.keys.get(self.index).and_then(|key| {
            self.dictionary.with_value(|value| match value {
                Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
                _ => None,
            })
        });
        self.current.rebind_cursor_value(target);
    }

    /// Return the current entry. At end, the key is empty and the value is an
    /// explicit uninitialized handle. The returned value shares the cursor's
    /// live value cell, matching qpdf's iterator `ivalue.second` reference.
    pub fn current(&mut self) -> DictItem {
        self.update_current();
        DictItem {
            key: self.keys.get(self.index).cloned().unwrap_or_default(),
            value: self.current.clone(),
        }
    }

    /// Advance one entry, retaining the end sentinel when already at end.
    pub fn next(&mut self) {
        self.index = self.index.saturating_add(1).min(self.len());
        self.update_current();
    }

    /// Move one entry backward, retaining the begin sentinel when already at
    /// the first entry.
    pub fn previous(&mut self) {
        self.index = self.index.saturating_sub(1);
        self.update_current();
    }

    /// Whether the cursor is at or beyond the current end.
    pub fn is_end(&self) -> bool {
        self.index >= self.len()
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

impl ObjectHandle {
    /// Construct qpdf's default, uninitialized handle.
    ///
    /// This is distinct from an initialized indirect handle whose value is
    /// still unresolved, and from an initialized null. Type predicates return
    /// false for it without attempting resolution; operations that require a
    /// value report qpdf's uninitialized-handle error at the dereference
    /// boundary (`include/qpdf/QPDFObjectHandle.hh:325-326`).
    pub fn uninitialized() -> Self {
        let handle = Self(empty_object_slot());
        handle.register_state_owner();
        handle
    }

    /// Whether this handle has a qpdf object allocation behind it.
    pub fn is_initialized(&self) -> bool {
        self.0.borrow().initialized
    }

    /// Rebind the stable value cell owned by an iterator cursor to the item it
    /// currently denotes. qpdf's C++ iterator stores an `ivalue` handle and
    /// assigns a new `QPDFObjectHandle` into that same member as the cursor
    /// moves; clones of the reference returned by `operator*` therefore see
    /// the later end/next-item assignment too
    /// (`libqpdf/QPDFObjectHandle.cc:2534-2542`).
    fn rebind_cursor_value(&self, target: Option<ObjectHandle>) {
        let old_owners = self.0.borrow().state_owners.clone();
        Self::remove_state_owner(&old_owners, &self.0);

        let target = target.map(|target| {
            let slot = target.0.borrow();
            (
                slot.initialized,
                slot.state.clone(),
                slot.state_owners.clone(),
                slot.object_ref,
                slot.active_pdf_unique_id,
                slot.resolver.clone(),
                slot.parsed_offset,
                slot.end_before_space,
                slot.end_after_space,
                slot.pdf_unique_ids.clone(),
                slot.tree_pdf_unique_id,
                slot.description.clone(),
                slot.stream_token_filters.clone(),
                slot.content_normalization_applied.clone(),
                slot.mutation_generation.clone(),
                slot.containment_parents.clone(),
            )
        });

        {
            let mut slot = self.0.borrow_mut();
            match target {
                Some((
                    initialized,
                    state,
                    state_owners,
                    object_ref,
                    active_pdf_unique_id,
                    resolver,
                    parsed_offset,
                    end_before_space,
                    end_after_space,
                    pdf_unique_ids,
                    tree_pdf_unique_id,
                    description,
                    stream_token_filters,
                    content_normalization_applied,
                    mutation_generation,
                    containment_parents,
                )) => {
                    slot.initialized = initialized;
                    slot.state = state;
                    slot.state_owners = state_owners;
                    slot.object_ref = object_ref;
                    slot.active_pdf_unique_id = active_pdf_unique_id;
                    slot.resolver = resolver;
                    slot.parsed_offset = parsed_offset;
                    slot.end_before_space = end_before_space;
                    slot.end_after_space = end_after_space;
                    slot.pdf_unique_ids = pdf_unique_ids;
                    slot.tree_pdf_unique_id = tree_pdf_unique_id;
                    slot.description = description;
                    slot.stream_token_filters = stream_token_filters;
                    slot.content_normalization_applied = content_normalization_applied;
                    slot.mutation_generation = mutation_generation;
                    // Preserve the target's own containment provenance so a
                    // cursor-derived direct child stays dirty-markable via
                    // Pdf::mark_object_handle_dirty (Codex Review finding on
                    // PR #1353; clearing it here silently dropped edits made
                    // through a cursor's current() handle).
                    slot.containment_parents = containment_parents;
                }
                None => {
                    slot.initialized = false;
                    slot.state = Rc::new(RefCell::new(ObjectValue::Unresolved));
                    slot.state_owners = Rc::new(RefCell::new(Vec::new()));
                    slot.object_ref = None;
                    slot.active_pdf_unique_id = None;
                    slot.resolver = None;
                    slot.parsed_offset = NO_PARSED_OFFSET;
                    slot.end_before_space = NO_PARSED_OFFSET;
                    slot.end_after_space = NO_PARSED_OFFSET;
                    slot.pdf_unique_ids.clear();
                    slot.tree_pdf_unique_id = None;
                    slot.containment_parents.clear();
                    slot.description = None;
                    slot.stream_token_filters = Rc::new(RefCell::new(Vec::new()));
                    slot.content_normalization_applied = Rc::new(Cell::new(false));
                    slot.mutation_generation = Rc::new(Cell::new(0));
                }
            }
        }
        self.register_state_owner();
    }

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
    /// The sentinel is represented as an `ObjectValue`, while the surrounding
    /// slot retains its indirect identity and resolver metadata
    /// (`include/qpdf/Constants.h:108-127`,
    /// `libqpdf/QPDF_Reserved.cc:1-27`). [`crate::Pdf::new_reserved`]'s own
    /// sentinel always has an indirect identity and document owner, but
    /// [`Self::shallow_copy`] on a reserved handle mirrors
    /// `QPDF_Reserved::copy` (`libqpdf/QPDF_Reserved.cc:14-19`) and produces
    /// a second, *direct* reserved handle with neither — so indirect
    /// identity is this sentinel's common case, not a universal one.
    pub fn is_reserved(&self) -> bool {
        let state = self.0.borrow().state.clone();
        let reserved = matches!(&*state.borrow(), ObjectValue::Reserved);
        reserved
    }

    /// The object number/generation for an indirect handle, or `None` for a
    /// direct one.
    pub fn object_ref(&self) -> Option<ObjectRef> {
        self.0.borrow().object_ref
    }

    /// The owning document identity carried by this canonical handle, if it
    /// was minted by a [`crate::Pdf`]. This is qpdf's source-QPDF identity
    /// key used by `QPDF::copyForeignObject`'s per-source `ObjCopier` map
    /// (`libqpdf/QPDF.cc:2065`).
    pub fn owning_pdf_unique_id(&self) -> Option<u64> {
        self.0.borrow().active_pdf_unique_id
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

    /// Return a stable set key for this canonical allocation.
    pub(crate) fn identity_key(&self) -> ObjectHandleIdentity {
        ObjectHandleIdentity(self.0.clone())
    }

    #[cfg(test)]
    fn ptr_eq(&self, other: &Self) -> bool {
        self.is_same_object_as(other)
    }

    /// An indirect slot without an owning document resolver. This is the
    /// detached handle shape used by syntax-only parser boundaries.
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
            initialized: true,
            state: Rc::new(RefCell::new(ObjectValue::Reserved)),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: Some(object_ref),
            active_pdf_unique_id: Some(pdf_unique_id),
            resolver: Some(resolver),
            parsed_offset: NO_PARSED_OFFSET,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            tree_pdf_unique_id: None,
            containment_parents: Vec::new(),
            description: None,
            stream_token_filters: Rc::new(RefCell::new(Vec::new())),
            content_normalization_applied: Rc::new(Cell::new(false)),
            mutation_generation: Rc::new(Cell::new(0)),
        })));
        handle.register_state_owner();
        handle
    }

    /// Construct a fresh, direct reserved sentinel with no object number and
    /// no document owner — the shape [`Self::shallow_copy`] needs to mirror
    /// `QPDF_Reserved::copy` (`libqpdf/QPDF_Reserved.cc:14-19`), which
    /// unconditionally returns `create()`: a brand-new `QPDF_Reserved`
    /// instance, wrapped by `QPDFObjectHandle::shallowCopy`
    /// (`libqpdf/QPDFObjectHandle.cc:2073-2079`) the same direct-handle way
    /// as any other type's `copy()` result. Unlike
    /// [`Self::new_reserved_for_pdf`] (the indirect, document-owned sentinel
    /// [`crate::Pdf::new_reserved`] hands out), this shares no identity, no
    /// object number, and no owning document with the handle it was copied
    /// from.
    pub(crate) fn new_reserved_direct() -> Self {
        let handle = Self(Rc::new(RefCell::new(ObjectSlot {
            initialized: true,
            state: Rc::new(RefCell::new(ObjectValue::Reserved)),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: None,
            active_pdf_unique_id: None,
            resolver: None,
            parsed_offset: NO_PARSED_OFFSET,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            tree_pdf_unique_id: None,
            containment_parents: Vec::new(),
            description: None,
            stream_token_filters: Rc::new(RefCell::new(Vec::new())),
            content_normalization_applied: Rc::new(Cell::new(false)),
            mutation_generation: Rc::new(Cell::new(0)),
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
            initialized: true,
            state: Rc::new(RefCell::new(ObjectValue::Unresolved)),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: Some(object_ref),
            active_pdf_unique_id: pdf_unique_id,
            resolver,
            parsed_offset: NO_PARSED_OFFSET,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            tree_pdf_unique_id: None,
            containment_parents: Vec::new(),
            description: None,
            stream_token_filters: Rc::new(RefCell::new(Vec::new())),
            content_normalization_applied: Rc::new(Cell::new(false)),
            mutation_generation: Rc::new(Cell::new(0)),
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
        Self::new_direct_with_resolver_inner(
            canonicalize_object_value(value),
            parsed_offset,
            resolver,
        )
    }

    /// Construct the result of qpdf's `shallowCopy` without rewriting raw
    /// dictionary keys. `QPDF_Name::normalizeName` preserves a key's first
    /// byte, so a caller that supplied `replaceKey("Array1", ...)` must still
    /// see that slashless key when the copied dictionary is later emitted.
    fn new_direct_preserving_dictionary_keys(value: ObjectValue, parsed_offset: i64) -> Self {
        Self::new_direct_with_resolver_inner(value, parsed_offset, None)
    }

    fn new_direct_with_resolver_inner(
        value: ObjectValue,
        parsed_offset: i64,
        resolver: Option<Weak<dyn DocumentResolver>>,
    ) -> Self {
        let handle = Self(Rc::new(RefCell::new(ObjectSlot {
            initialized: true,
            state: Rc::new(RefCell::new(value)),
            state_owners: Rc::new(RefCell::new(Vec::new())),
            object_ref: None,
            active_pdf_unique_id: None,
            resolver,
            parsed_offset,
            end_before_space: NO_PARSED_OFFSET,
            end_after_space: NO_PARSED_OFFSET,
            pdf_unique_ids: BTreeSet::new(),
            tree_pdf_unique_id: None,
            containment_parents: Vec::new(),
            description: None,
            stream_token_filters: Rc::new(RefCell::new(Vec::new())),
            content_normalization_applied: Rc::new(Cell::new(false)),
            mutation_generation: Rc::new(Cell::new(0)),
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

    fn state_children(state: &ObjectValue) -> Vec<ObjectHandle> {
        Self::direct_children(state)
    }

    /// Replace the shared payload and keep every slot that owns it in sync
    /// with the payload's direct-child containment edges.
    fn replace_shared_state(&self, new_state: ObjectValue) -> ObjectValue {
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
        let new_token_filters = Rc::new(RefCell::new(Vec::new()));
        let new_content_normalization_applied = Rc::new(Cell::new(false));
        let new_mutation_generation = Rc::new(Cell::new(0));
        for owner in self.state_owner_handles() {
            let parent = owner.containment_parent();
            {
                let mut owner_slot = owner.0.borrow_mut();
                owner_slot.stream_token_filters = new_token_filters.clone();
                owner_slot.content_normalization_applied =
                    new_content_normalization_applied.clone();
                owner_slot.mutation_generation = new_mutation_generation.clone();
            }
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
    fn replace_detached_state(&self, new_state: ObjectValue) {
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
            slot.stream_token_filters = Rc::new(RefCell::new(Vec::new()));
            slot.content_normalization_applied = Rc::new(Cell::new(false));
            slot.mutation_generation = Rc::new(Cell::new(0));
        }
        self.register_state_owner();

        let parent = self.containment_parent();
        for child in &old_children {
            Self::detach_child_from_parent(child, &parent);
        }
    }

    /// Validate the source-state contract for qpdf-shaped replacement.
    ///
    /// flpdf currently accepts only a direct handle with a resolved value at
    /// this boundary. Keeping this check separate lets the resolver run it
    /// before minting an absent target cache entry. qpdf's
    /// `QPDF::replaceObject` rejects both an indirect and an uninitialized
    /// handle with the same message
    /// (`libqpdf/QPDF.cc:1986-1989`: `if (oh.isIndirect() ||
    /// !oh.isInitialized())`).
    pub(crate) fn validate_replacement_source(&self) -> Result<()> {
        if !self.is_direct() {
            return Err(crate::Error::Unsupported(
                "replacement ObjectHandle must be direct".to_string(),
            ));
        }
        if !self.is_initialized() {
            return Err(crate::Error::Unsupported(
                "QPDF::replaceObject called with indirect object handle".to_string(),
            ));
        }
        Ok(())
    }

    /// Make `self` and a distinct direct replacement handle observe one
    /// shared payload, preserving the canonical target slot's identity.
    ///
    /// qpdf's `QPDFObject::assign` shares the `QPDFValue` allocation rather
    /// than copying it (`QPDFObject_private.hh:117-120`). The two Rust
    /// [`ObjectHandle`] slots therefore retain separate object metadata while
    /// sharing the payload and its mutation visibility.
    pub(crate) fn share_value_state_with(&self, source: &Self) -> Result<()> {
        if self.is_same_object_as(source) {
            return Ok(());
        }
        source.validate_replacement_source()?;
        let (
            source_state,
            source_owners,
            source_token_filters,
            source_content_normalization_applied,
            source_mutation_generation,
        ) = {
            let source_slot = source.0.borrow();
            (
                source_slot.state.clone(),
                source_slot.state_owners.clone(),
                source_slot.stream_token_filters.clone(),
                source_slot.content_normalization_applied.clone(),
                source_slot.mutation_generation.clone(),
            )
        };
        let old_state = {
            let mut target = self.0.borrow_mut();
            let old_state = target.state.clone();
            let old_owners = target.state_owners.clone();
            Self::remove_state_owner(&old_owners, &self.0);
            target.state = source_state.clone();
            target.state_owners = source_owners.clone();
            target.stream_token_filters = source_token_filters;
            target.content_normalization_applied = source_content_normalization_applied;
            target.mutation_generation = source_mutation_generation;
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
    #[cfg(test)]
    pub(crate) fn remove_from_document(&self) {
        if !self.is_indirect() {
            return;
        }
        self.replace_detached_state(ObjectValue::Null);
        let mut slot = self.0.borrow_mut();
        slot.object_ref = None;
        slot.active_pdf_unique_id = None;
        slot.tree_pdf_unique_id = None;
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
    pub(crate) fn promote_to_indirect(
        &self,
        object_ref: ObjectRef,
        pdf_unique_id: u64,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        let children = {
            let mut slot = self.0.borrow_mut();
            slot.initialized = true;
            slot.object_ref = Some(object_ref);
            slot.active_pdf_unique_id = Some(pdf_unique_id);
            slot.resolver = Some(resolver);
            slot.pdf_unique_ids.insert(pdf_unique_id);
            let state = slot.state.borrow();
            Self::direct_children(&state)
        };
        let mut visited = BTreeSet::new();
        visited.insert(Rc::as_ptr(&self.0) as usize);
        for child in children {
            child.associate_pdf_identity(pdf_unique_id, &mut visited);
        }
        self.clone()
    }

    /// Construct a direct handle wrapping an already-built [`ObjectValue`], at
    /// the no-offset sentinel. Used by parser and bootstrap boundaries to wrap
    /// an already-decoded internal value without going through one of the
    /// typed public factories above.
    pub(crate) fn from_value(value: ObjectValue) -> Self {
        Self::new_direct(value, NO_PARSED_OFFSET)
    }

    /// Construct a direct value with a weak resolver context. The resolver is
    /// intentionally weak so a direct value does not keep its document alive;
    /// parser-created values use [`Self::from_parsed_value_with_resolver`] to
    /// add qpdf's per-value document identity as well.
    pub(crate) fn from_value_with_resolver(
        value: ObjectValue,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        Self::new_direct_with_resolver(value, NO_PARSED_OFFSET, Some(resolver))
    }

    /// Construct a parser-created direct value with the owning document's
    /// weak resolver and identity, matching qpdf's per-value `QPDF*`
    /// association (`QPDFValue.hh:60-66,149-152`). Programmatic direct values
    /// deliberately use [`Self::from_value_with_resolver`] instead: qpdf does
    /// not assign an owner to a newly constructed direct array merely because
    /// it is inserted into an indirect dictionary.
    pub(crate) fn from_parsed_value_with_resolver(
        value: ObjectValue,
        resolver: Weak<dyn DocumentResolver>,
    ) -> Self {
        let pdf_unique_id = resolver
            .upgrade()
            .and_then(|resolver| resolver.pdf_unique_id());
        let handle = Self::from_value_with_resolver(value, resolver);
        if !handle.is_null() {
            handle.0.borrow_mut().active_pdf_unique_id = pdf_unique_id;
        }
        handle
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
    pub(crate) fn into_direct_value(mut self) -> Option<(ObjectValue, i64)> {
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
                value @ (ObjectValue::Unresolved
                | ObjectValue::Reserved
                | ObjectValue::Destroyed) => {
                    let _ = value;
                    return None;
                }
                value => Self::direct_children(value),
            }
        };
        for child in children {
            Self::detach_child_from_parent(&child, &parent);
        }
        let slot = Rc::try_unwrap(std::mem::replace(&mut self.0, empty_object_slot()))
            .ok()?
            .into_inner();
        let state = Rc::try_unwrap(slot.state).ok()?.into_inner();
        Some((state, slot.parsed_offset))
    }

    /// Legacy cloning helper for the unchanged public
    /// `Pdf::make_indirect_object_handle` allocator: returns a direct value
    /// copy, or `None` for an indirect handle. It is not the qpdf-native
    /// promotion primitive — qpdf promotes by registering and updating the
    /// existing `QPDFObject` allocation (`libqpdf/QPDF.cc:1835-1839,1882-1897`).
    /// Canonical callers use [`Self::promote_to_indirect`]; this helper remains
    /// for the legacy public allocator whose copy semantics are part of its
    /// existing contract.
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
    /// swapping the other's bytes — the copied object would describe payload
    /// it does not hold. The stream dictionary is privatized like any other
    /// direct child so each copied slot stays internally consistent; the
    /// payload `Rc` is shared, which is safe because replacing it swaps a
    /// field rather than mutating the buffer.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::shallow_copy`]'s stream rejection when the stream
    /// dictionary being privatized itself holds a *direct* stream, the same
    /// case `QPDF_Dictionary::copy` throws on.
    ///
    /// Also returns [`Error::Unsupported`] for a *direct* reserved handle.
    /// This legacy promotion helper is intentionally narrower than qpdf's
    /// `replaceObject`/`shallowCopy` value layer; the canonical promotion
    /// primitive is [`Self::promote_to_indirect`].
    pub(crate) fn direct_value_clone(&self) -> Result<Option<ObjectValue>> {
        let slot = self.0.borrow();
        if slot.object_ref.is_some() {
            return Ok(None);
        }
        let state = slot.state.borrow();
        Ok(Some(match &*state {
            ObjectValue::Reserved => return Err(reserved_clone_error()),
            ObjectValue::Unresolved | ObjectValue::Destroyed => return Ok(None),
            ObjectValue::Stream {
                stream_dict,
                stream_data,
                stream_provider,
                filter_on_write,
                stream_length,
            } => ObjectValue::Stream {
                stream_dict: shallow_copy_child(stream_dict)?,
                stream_data: stream_data.clone(),
                stream_provider: stream_provider.clone(),
                filter_on_write: *filter_on_write,
                stream_length: *stream_length,
            },
            other => other.clone(),
        }))
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
    ///
    /// qpdf's null cache update carries `-1` source positions. Reset the
    /// provenance before a parser can install a real offset for a parsed
    /// literal null (`libqpdf/QPDF.cc:1706-1749,1843-1858`).
    pub(crate) fn set_resolved(&self, value: ObjectValue) {
        if self.is_indirect() {
            self.0.borrow_mut().initialized = true;
            let value = canonicalize_object_value(value);
            let is_null = matches!(value, ObjectValue::Null);
            self.replace_shared_state(value);
            if is_null {
                let mut slot = self.0.borrow_mut();
                slot.parsed_offset = NO_PARSED_OFFSET;
                slot.end_before_space = NO_PARSED_OFFSET;
                slot.end_after_space = NO_PARSED_OFFSET;
                slot.description = None;
            }
        }
    }

    /// Rebind only this slot to a null value while leaving its indirect
    /// identity in place. This is the compatibility-delete transition: qpdf
    /// detaches the cache object before nulling it (`QPDF.cc:1996-2005`), so a
    /// replacement alias must not observe the target's null mutation.
    pub(crate) fn detach_value_to_null(&self) {
        if self.is_indirect() {
            self.replace_detached_state(ObjectValue::Null);
            let mut slot = self.0.borrow_mut();
            slot.parsed_offset = NO_PARSED_OFFSET;
            slot.end_before_space = NO_PARSED_OFFSET;
            slot.end_after_space = NO_PARSED_OFFSET;
            slot.description = None;
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

    /// True if every indirect descendant reachable through `self`'s direct
    /// value graph (stopping at each indirect boundary) belongs to
    /// `pdf_unique_id`, or carries no recorded document identity at all.
    ///
    /// Unlike qpdf's own `checkOwnership`
    /// (`libqpdf/QPDFObjectHandle.cc:2355-2365`, `QPDF_Array.cc:10-26`),
    /// which is a shallow, O(1) comparison of only the top-level handle's
    /// own owning document, this walks the complete direct-value descendant
    /// graph. It exists for resolver-side replacement validation as a
    /// flpdf-specific defense against a foreign indirect object nested several
    /// direct hops below a replacement value -- a shape qpdf's own shallow
    /// check does not catch. [`Self::check_key_value_ownership`] (the
    /// `replace_key` and array mutator ownership boundary) intentionally does
    /// not call this: qpdf's real `checkOwnership` never does either.
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

    /// Claim this shared handle as the root of a name/number tree in one
    /// document. A wrapper stores its own fast-path claim, but clones of the
    /// same contextless root must observe the first wrapper's claim as well.
    /// This is the Rust-side equivalent of qpdf's helper retaining its owning
    /// `QPDF` next to the shared `QPDFObjectHandle`.
    pub(crate) fn claim_tree_pdf(&self, pdf_unique_id: u64) -> Result<()> {
        let mut slot = self.0.borrow_mut();
        match slot.tree_pdf_unique_id {
            None => {
                slot.tree_pdf_unique_id = Some(pdf_unique_id);
                Ok(())
            }
            Some(owner) if owner == pdf_unique_id => Ok(()),
            Some(_) => Err(Error::Unsupported(
                "name/number tree root belongs to a different Pdf".to_string(),
            )),
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
    /// (`libqpdf/QPDF.cc`, `QPDF::~QPDF`). Literal null values stay null. The
    /// reader's `Pdf::drop` calls this for every entry in its handle
    /// registry — the sole owner of the canonical `Rc`s — before the registry
    /// itself is dropped, so no lingering cycle keeps a document's object
    /// graph (and any reachable stream buffers) alive past the `Pdf` that
    /// produced it.
    ///
    /// Resets the parsed offset to the no-offset sentinel only when the value
    /// is destroyed. Surviving null values retain their existing parsed-offset
    /// provenance.
    pub(crate) fn disconnect(&self) {
        let should_destroy = {
            let slot = self.0.borrow();
            if slot.object_ref.is_none() {
                return;
            }
            let state = slot.state.borrow();
            !matches!(&*state, ObjectValue::Null)
        };
        if should_destroy {
            self.replace_detached_state(ObjectValue::Destroyed);
        }
        let mut slot = self.0.borrow_mut();
        slot.object_ref = None;
        slot.active_pdf_unique_id = None;
        slot.tree_pdf_unique_id = None;
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
        let resolved = !matches!(&*state.borrow(), ObjectValue::Unresolved);
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
            if !slot.initialized {
                return Err(Error::Internal(
                    "attempted to dereference an uninitialized QPDFObjectHandle".to_owned(),
                ));
            }
            let Some(object_ref) = slot.object_ref else {
                return Ok(());
            };
            let state = slot.state.borrow();
            if !matches!(&*state, ObjectValue::Unresolved) {
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
    pub(crate) fn context(&self) -> Option<Rc<dyn DocumentResolver>> {
        // qpdf's `setChildDescription` copies the owning QPDF onto the child
        // (`libqpdf/QPDFObject_private.hh:79-91`). A missing-key null can be
        // nested through several dictionary lookups, so follow the complete
        // child-description chain rather than checking only its immediate
        // parent. Keep a slot-identity guard because malformed programmatic
        // direct values can construct reciprocal description links.
        let mut current = self.clone();
        let mut seen = BTreeSet::new();
        loop {
            let current_id = Rc::as_ptr(&current.0) as usize;
            if !seen.insert(current_id) {
                break;
            }

            let (resolver, parent) = {
                let slot = current.0.borrow();
                let resolver = slot.resolver.as_ref().and_then(Weak::upgrade);
                let parent = match &slot.description {
                    Some(ObjectDescription::Child(child)) => child.parent.upgrade(),
                    _ => None,
                };
                (resolver, parent)
            };
            if let Some(resolver) = resolver {
                return Some(resolver);
            }
            let Some(parent) = parent else {
                break;
            };
            current = ObjectHandle(parent);
        }

        // qpdf's literal `QPDFObjectHandle::newNull()` carries neither a
        // QPDF* nor a resolver, even when it is nested below a document-owned
        // array, dictionary, or stream dictionary
        // (`libqpdf/QPDF_Null.cc:12-15`, `QPDFParser.cc:397-410`). Do not lend
        // it the parent's context through our containment back-links: a
        // contextless null must take qpdf's exception path. Non-null direct
        // children still use the containment-parent fallback below, and
        // indirect nulls retain their own resolver above.
        let slot = self.0.borrow();
        if matches!(&*slot.state.borrow(), ObjectValue::Null) {
            return None;
        }
        slot.containment_parents.iter().find_map(|parent| {
            parent
                .upgrade()
                .and_then(|p| p.borrow().resolver.as_ref().and_then(Weak::upgrade))
        })
    }

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

    /// Attach a qpdf document and explicit description to this shared object
    /// allocation, mirroring `QPDFObjectHandle::setObjectDescription`
    /// (`libqpdf/QPDFObjectHandle.cc:2056-2063`) and
    /// `QPDFValue::setDescription` (`libqpdf/qpdf/QPDFValue.hh:60-66`).
    ///
    /// This operation does not resolve or clone the handle. It changes the
    /// warning context seen by every alias of the same handle, just as qpdf
    /// stores the document pointer and description on the shared
    /// `QPDFValue`. The supplied `Pdf` must remain alive while the handle may
    /// emit a warning; the weak resolver preserves that lifetime boundary.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] if the document's canonical resolver link
    /// is no longer available.
    pub fn set_object_description<R: std::io::Read + std::io::Seek + 'static>(
        &self,
        pdf: &crate::Pdf<R>,
        description: impl Into<String>,
    ) -> Result<()> {
        let resolver = pdf.resolver.document_resolver_weak()?;
        let mut slot = self.0.borrow_mut();
        slot.resolver = Some(resolver);
        slot.active_pdf_unique_id = Some(pdf.unique_id);
        slot.description = Some(ObjectDescription::Template(description.into()));
        Ok(())
    }

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
    pub(crate) fn type_warning(&self, expected_type: &str, warning: &str) -> Result<()> {
        self.try_dereference()?;
        let desc = self.description();
        let prefix = if desc.is_empty() {
            String::new()
        } else {
            format!("{desc}: ")
        };
        let type_name = self.type_name()?;
        self.warn_through_context(format!(
            "{prefix}operation for {expected_type} attempted on object of type {}: {warning}",
            type_name
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
    pub fn try_get_keys(&self) -> Result<BTreeSet<Vec<u8>>> {
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
    pub(crate) fn try_as_name(&self) -> Result<Option<Vec<u8>>> {
        self.try_dereference()?;
        Ok(self.as_name())
    }

    /// True when this handle lazily resolves to the requested decoded name.
    ///
    /// Ports `QPDFObjectHandle::isNameAndEquals`
    /// (`libqpdf/QPDFObjectHandle.cc:456-459`). qpdf's canonical name string
    /// includes its leading slash; the internal name representation follows
    /// this crate's existing convention and stores the same decoded bytes
    /// without it.
    pub fn try_is_name_and_equals(&self, name: &[u8]) -> Result<bool> {
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
    pub fn try_is_or_has_name(&self, name: &[u8]) -> Result<bool> {
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
    pub fn try_is_dictionary_of_type(&self, type_name: &[u8], subtype_name: &[u8]) -> Result<bool> {
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

    /// True when this handle lazily resolves to a stream whose own
    /// dictionary's optional `/Type` and `/Subtype` names equal the
    /// requested decoded bytes.
    ///
    /// Ports `QPDFObjectHandle::isStreamOfType`
    /// (`libqpdf/QPDFObjectHandle.cc:468-471`): `isStream() &&
    /// getDict().isDictionaryOfType(type, subtype)`. Unlike
    /// [`Self::try_is_dictionary_of_type`], which only matches a plain
    /// dictionary value, this matches a stream's *nested* dictionary --
    /// the shape every `/Type /ObjStm` or `/Type /XRef` object actually
    /// has, since both are required to carry stream data.
    pub(crate) fn try_is_stream_of_type(
        &self,
        type_name: &[u8],
        subtype_name: &[u8],
    ) -> Result<bool> {
        self.try_dereference()?;
        let Some(stream_dict) = self.as_stream_dict() else {
            return Ok(false);
        };
        stream_dict.try_is_dictionary_of_type(type_name, subtype_name)
    }

    /// qpdf-compatible array inspection with lazy dereference. Only the array
    /// itself is resolved; each returned child keeps its own identity.
    pub(crate) fn try_as_array(&self) -> Result<Option<Vec<ObjectHandle>>> {
        if !self.is_initialized() {
            return Ok(None);
        }
        self.try_dereference()?;
        Ok(self.as_array())
    }

    /// Test whether the resolved value is an integer. An uninitialized qpdf
    /// handle has no value and therefore returns false without a warning.
    pub fn try_is_integer(&self) -> Result<bool> {
        if !self.is_initialized() {
            return Ok(false);
        }
        self.try_dereference()?;
        Ok(self.as_integer().is_some())
    }

    /// Test whether the resolved value is an array.
    pub fn try_is_array(&self) -> Result<bool> {
        if !self.is_initialized() {
            return Ok(false);
        }
        self.try_dereference()?;
        Ok(self.as_array().is_some())
    }

    /// Test whether the resolved value is a dictionary.
    pub fn try_is_dictionary(&self) -> Result<bool> {
        if !self.is_initialized() {
            return Ok(false);
        }
        self.try_dereference()?;
        Ok(self.as_dictionary().is_some())
    }

    /// Test whether the resolved value is a name.
    pub fn try_is_name(&self) -> Result<bool> {
        if !self.is_initialized() {
            return Ok(false);
        }
        self.try_dereference()?;
        Ok(self.as_name().is_some())
    }

    /// Test whether the resolved value is a scalar in qpdf's sense: boolean,
    /// integer, name, null, real, or string
    /// (`libqpdf/QPDFObjectHandle.cc:449-453`).
    pub fn try_is_scalar(&self) -> Result<bool> {
        if !self.is_initialized() {
            return Ok(false);
        }
        is_scalar(self)
    }

    /// Test whether the resolved value is an integer or real number.
    pub fn try_is_number(&self) -> Result<bool> {
        if !self.is_initialized() {
            return Ok(false);
        }
        self.try_dereference()?;
        Ok(self.as_integer().is_some() || self.as_real().is_some())
    }

    /// Create a cursor view over this array after resolving the receiver.
    pub fn try_array_items(&self) -> Result<ArrayItems> {
        self.try_dereference()?;
        Ok(ArrayItems {
            array: self.clone(),
        })
    }

    /// Create a cursor view over this dictionary after resolving the
    /// receiver.
    pub fn try_dict_items(&self) -> Result<DictItems> {
        self.try_dereference()?;
        let mut keys = Vec::new();
        if let Some(entries) = self.as_dictionary() {
            for (key, value) in entries {
                if !value.try_is_null()? {
                    keys.push(key);
                }
            }
        }
        Ok(DictItems {
            dictionary: self.clone(),
            keys: Rc::new(keys),
        })
    }

    fn try_get_value_as_number(&self) -> Result<Option<f64>> {
        if !self.is_initialized() {
            return Ok(None);
        }
        self.try_dereference()?;
        Ok(self.with_value(|value| match value {
            Some(ObjectValue::Integer(value)) => Some(*value as f64),
            Some(ObjectValue::Real(value)) => Some(*value),
            Some(ObjectValue::RealLiteral { value, .. }) => Some(*value),
            _ => None,
        }))
    }

    /// Construct a four-item rectangle array owned by this canonical handle
    /// layer (`libqpdf/QPDFObjectHandle.cc:1987-1991`).
    pub fn new_from_rectangle(rectangle: Rectangle) -> Self {
        Self::array(vec![
            Self::real(rectangle.llx),
            Self::real(rectangle.lly),
            Self::real(rectangle.urx),
            Self::real(rectangle.ury),
        ])
    }

    /// Return whether this handle is exactly a four-number array without
    /// emitting type warnings (`libqpdf/QPDFObjectHandle.cc:789-799`).
    pub fn try_is_rectangle(&self) -> Result<bool> {
        let Some(items) = self.try_as_array()? else {
            return Ok(false);
        };
        if items.len() != 4 {
            return Ok(false);
        }
        for item in items {
            if !item.try_is_number()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Convert a four-number array to qpdf's normalized rectangle, returning
    /// the zero rectangle for an invalid shape
    /// (`libqpdf/QPDFObjectHandle.cc:801-824`).
    pub fn try_get_array_as_rectangle(&self) -> Result<Rectangle> {
        let Some(items) = self.try_as_array()? else {
            return Ok(Rectangle::default());
        };
        if items.len() != 4 {
            return Ok(Rectangle::default());
        }
        let mut values = [0.0; 4];
        for (index, item) in items.into_iter().enumerate() {
            let Some(value) = item.try_get_value_as_number()? else {
                return Ok(Rectangle::default());
            };
            values[index] = value;
        }
        Ok(Rectangle::new(
            values[0].min(values[2]),
            values[1].min(values[3]),
            values[0].max(values[2]),
            values[1].max(values[3]),
        ))
    }

    /// Construct a six-item qpdf object-handle matrix array.
    pub fn new_from_matrix(matrix: ObjectHandleMatrix) -> Self {
        Self::array(vec![
            Self::real(matrix.a),
            Self::real(matrix.b),
            Self::real(matrix.c),
            Self::real(matrix.d),
            Self::real(matrix.e),
            Self::real(matrix.f),
        ])
    }

    /// Construct a six-item array from flpdf's standalone `QPDFMatrix`
    /// equivalent. This is the second qpdf `newFromMatrix` overload; its
    /// identity-default behavior belongs to [`crate::Matrix`], not to the
    /// nested [`ObjectHandleMatrix`]
    /// (`include/qpdf/QPDFObjectHandle.hh:254-285`).
    pub fn new_from_qpdf_matrix(matrix: crate::Matrix) -> Self {
        Self::array(vec![
            Self::real(matrix.a),
            Self::real(matrix.b),
            Self::real(matrix.c),
            Self::real(matrix.d),
            Self::real(matrix.e),
            Self::real(matrix.f),
        ])
    }

    /// Return whether this handle is exactly a six-number array without
    /// emitting type warnings (`libqpdf/QPDFObjectHandle.cc:801-811`).
    pub fn try_is_matrix(&self) -> Result<bool> {
        let Some(items) = self.try_as_array()? else {
            return Ok(false);
        };
        if items.len() != 6 {
            return Ok(false);
        }
        for item in items {
            if !item.try_is_number()? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Convert a six-number array to the qpdf nested matrix type, returning
    /// its all-zero default for an invalid shape
    /// (`libqpdf/QPDFObjectHandle.cc:826-853`).
    pub fn try_get_array_as_matrix(&self) -> Result<ObjectHandleMatrix> {
        let Some(items) = self.try_as_array()? else {
            return Ok(ObjectHandleMatrix::default());
        };
        if items.len() != 6 {
            return Ok(ObjectHandleMatrix::default());
        }
        let mut values = [0.0; 6];
        for (index, item) in items.into_iter().enumerate() {
            let Some(value) = item.try_get_value_as_number()? else {
                return Ok(ObjectHandleMatrix::default());
            };
            values[index] = value;
        }
        Ok(ObjectHandleMatrix::new(
            values[0], values[1], values[2], values[3], values[4], values[5],
        ))
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
    pub(crate) fn try_array_item(&self, index: usize) -> Result<Option<ObjectHandle>> {
        self.try_dereference()?;
        Ok(self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => children.get(index).cloned(),
            _ => None,
        }))
    }

    /// Return qpdf's array length, warning and treating a non-array as empty
    /// (`libqpdf/QPDFObjectHandle.cc:758-768`).
    pub fn try_get_array_n_items(&self) -> Result<usize> {
        self.try_dereference()?;
        match self.with_value(|value| match value {
            Some(ObjectValue::Array(children)) => Some(children.len()),
            _ => None,
        }) {
            Some(length) => Ok(length),
            None => {
                self.type_warning("array", "treating as empty")?;
                Ok(0)
            }
        }
    }

    /// Return one array item using qpdf's signed-index domain. A non-array
    /// warns with the array fallback; an invalid index on an array warns at
    /// the object boundary and returns a contextual null
    /// (`libqpdf/QPDFObjectHandle.cc:770-785`).
    pub fn try_get_array_item(&self, index: i64) -> Result<ObjectHandle> {
        self.try_dereference()?;
        let item = self.with_value(|value| {
            let Some(ObjectValue::Array(children)) = value else {
                return Err(false);
            };
            let Some(index) = usize::try_from(index).ok() else {
                return Err(true);
            };
            children.get(index).cloned().ok_or(true)
        });
        match item {
            Ok(item) => Ok(item),
            Err(true) => {
                self.object_warning("returning null for out of bounds array access")?;
                let null = ObjectHandle::null();
                null.set_child_description(self, " -> null returned from invalid array access", "");
                Ok(null)
            }
            Err(false) => {
                self.type_warning("array", "returning null")?;
                let null = ObjectHandle::null();
                null.set_child_description(self, " -> null returned from invalid array access", "");
                Ok(null)
            }
        }
    }

    /// Return the array children, warning and treating a non-array as an
    /// empty vector (`libqpdf/QPDFObjectHandle.cc:787-801`).
    pub fn try_get_array_as_vector(&self) -> Result<Vec<ObjectHandle>> {
        self.try_dereference()?;
        match self.as_array() {
            Some(items) => Ok(items),
            None => {
                self.type_warning("array", "treating as empty")?;
                Ok(Vec::new())
            }
        }
    }

    /// qpdf-compatible integer inspection with lazy dereference.
    ///
    /// Ports `QPDFObjectHandle::asInteger`, the silent internal helper.
    /// [`Self::try_get_int_value`] is the accessor that warns.
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
    /// document — the error reported by the internal type-warning boundary
    /// appears in place of the warning.
    pub fn try_get_int_value(&self) -> Result<i64> {
        match self.try_as_integer()? {
            Some(value) => Ok(value),
            None => {
                self.type_warning("integer", "returning 0")?;
                Ok(0)
            }
        }
    }

    /// Return qpdf's boolean value, warning and yielding `false` on any
    /// other type (`libqpdf/QPDFObjectHandle.cc:474-487`).
    pub fn try_get_bool_value(&self) -> Result<bool> {
        self.try_dereference()?;
        match self.as_boolean() {
            Some(value) => Ok(value),
            None => {
                self.type_warning("boolean", "returning false")?;
                Ok(false)
            }
        }
    }

    /// Return qpdf's source spelling for a real, warning and yielding `0.0`
    /// for any other type (`libqpdf/QPDFObjectHandle.cc:542-560`).
    pub fn try_get_real_value(&self) -> Result<Vec<u8>> {
        self.try_dereference()?;
        let value = self.with_value(|value| match value {
            Some(ObjectValue::RealLiteral { literal, .. }) => Some(literal.clone()),
            Some(ObjectValue::Real(value)) => Some(value.to_string().into_bytes()),
            _ => None,
        });
        match value {
            Some(value) => Ok(value),
            None => {
                self.type_warning("real", "returning 0.0")?;
                Ok(b"0.0".to_vec())
            }
        }
    }

    /// Return qpdf's numeric value for an integer or real, warning and
    /// yielding `0.0` for any other type (`libqpdf/QPDFObjectHandle.cc:371-389`).
    pub fn try_get_numeric_value(&self) -> Result<f64> {
        self.try_dereference()?;
        let value = self.with_value(|value| match value {
            Some(ObjectValue::Integer(value)) => Some(*value as f64),
            Some(ObjectValue::Real(value)) => Some(*value),
            Some(ObjectValue::RealLiteral { value, .. }) => Some(*value),
            _ => None,
        });
        match value {
            Some(value) => Ok(value),
            None => {
                self.type_warning("number", "returning 0")?;
                Ok(0.0)
            }
        }
    }

    /// Return qpdf's canonical slash-prefixed name, warning and yielding
    /// `/QPDFFakeName` for any other type (`libqpdf/QPDFObjectHandle.cc:542-584`).
    pub fn try_get_name(&self) -> Result<Vec<u8>> {
        self.try_dereference()?;
        match self.as_name() {
            Some(value) => {
                let mut result = Vec::with_capacity(value.len() + 1);
                result.push(b'/');
                result.extend_from_slice(&value);
                Ok(result)
            }
            None => {
                self.type_warning("name", "returning dummy name")?;
                Ok(b"/QPDFFakeName".to_vec())
            }
        }
    }

    /// Return stored string bytes, warning and yielding an empty string for
    /// any other type (`libqpdf/QPDFObjectHandle.cc:587-611`).
    pub fn try_get_string_value(&self) -> Result<Vec<u8>> {
        self.try_dereference()?;
        match self.as_string() {
            Some(value) => Ok(value),
            None => {
                self.type_warning("string", "returning empty string")?;
                Ok(Vec::new())
            }
        }
    }

    /// Return qpdf's UTF-8 string view, warning and yielding an empty string
    /// for any other type (`libqpdf/QPDFObjectHandle.cc:613-640`).
    pub fn try_get_utf8_value(&self) -> Result<Vec<u8>> {
        self.try_dereference()?;
        match self.as_string() {
            Some(value) => Ok(utf8_value(&value)),
            None => {
                self.type_warning("string", "returning empty string")?;
                Ok(Vec::new())
            }
        }
    }

    /// Return an operator's bytes, warning and yielding qpdf's fake value for
    /// any other type (`libqpdf/QPDFObjectHandle.cc:642-666`).
    pub fn try_get_operator_value(&self) -> Result<Vec<u8>> {
        self.try_dereference()?;
        match self.as_operator() {
            Some(value) => Ok(value),
            None => {
                self.type_warning("operator", "returning fake value")?;
                Ok(b"QPDFFAKE".to_vec())
            }
        }
    }

    /// Return inline-image bytes, warning and yielding an empty buffer for
    /// any other type (`libqpdf/QPDFObjectHandle.cc:668-690`).
    pub fn try_get_inline_image_value(&self) -> Result<Vec<u8>> {
        self.try_dereference()?;
        match self.as_inline_image() {
            Some(value) => Ok(value),
            None => {
                self.type_warning("inlineimage", "returning empty data")?;
                Ok(Vec::new())
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
    /// `warn_if_possible`, which — unlike `type_warning` —
    /// usually reports no error of its own; but a *reachable* document whose
    /// warning sink itself fails (no default logger sink, a resolver with no
    /// warn receiver) still propagates that failure here, and the saturated
    /// value is not returned in that case.
    pub fn try_get_int_value_as_int(&self) -> Result<i32> {
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

    /// Return qpdf's unsigned-long-long integer value, warning and yielding
    /// `0` for a negative integer (`libqpdf/QPDFObjectHandle.cc:556-575`).
    /// Non-integer values use [`Self::try_get_int_value`]'s zero fallback,
    /// so they emit only that type warning.
    pub fn try_get_uint_value(&self) -> Result<u64> {
        let value = self.try_get_int_value()?;
        if value < 0 {
            self.warn_if_possible("unsigned value request for negative number; returning 0")?;
            Ok(0)
        } else {
            // A non-negative i64 always fits in qpdf's unsigned long long
            // counterpart on the supported 64-bit targets.
            Ok(value as u64)
        }
    }

    /// Return qpdf's unsigned-int integer value, warning and saturating at
    /// `0`/`u32::MAX` when the signed value is outside that range
    /// (`libqpdf/QPDFObjectHandle.cc:580-604`).
    /// Non-integer values use [`Self::try_get_int_value`]'s zero fallback,
    /// so they emit only that type warning.
    pub fn try_get_uint_value_as_uint(&self) -> Result<u32> {
        let value = self.try_get_int_value()?;
        if value < 0 {
            let warning = "unsigned integer value request for negative number; returning 0";
            self.warn_if_possible(warning)?;
            Ok(0)
        } else if value > i64::from(u32::MAX) {
            let warning = "requested value of unsigned integer is too big; returning UINT_MAX";
            self.warn_if_possible(warning)?;
            Ok(u32::MAX)
        } else {
            Ok(value as u32)
        }
    }

    /// qpdf-compatible dictionary lookup. The holder dictionary is resolved;
    /// the returned child retains its own direct/indirect identity.
    ///
    /// Ports `QPDFObjectHandle::getKey`
    /// (`libqpdf/QPDFObjectHandle.cc:978-989`). A non-dictionary receiver
    /// yields null. qpdf additionally raises
    /// `typeWarning("dictionary", "returning null for attempted key
    /// retrieval")` at `:984`, and gives that null the
    /// `" -> null returned from getting key $VD from non-Dictionary"`
    /// child description. A dictionary's missing-key null instead carries
    /// the key-specific description from `QPDF_Dictionary::getKey`.
    ///
    /// `key` must be qpdf's decoded, canonical dictionary key including its
    /// leading `/` (for example, `/Type`). Lookup is exact; a slashless key is
    /// not an alias and is treated as missing.
    ///
    /// # Errors
    ///
    /// Propagates resolution failures.
    pub fn try_get_key(&self, key: &[u8]) -> Result<ObjectHandle> {
        self.try_dereference()?;
        let (is_dictionary, child) = self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => (true, entries.get(key).cloned()),
            _ => (false, None),
        });
        if let Some(child) = child {
            Ok(child)
        } else if !is_dictionary {
            self.type_warning("dictionary", "returning null for attempted key retrieval")?;
            let null = ObjectHandle::null();
            null.set_child_description(
                self,
                " -> null returned from getting key $VD from non-Dictionary",
                "",
            );
            Ok(null)
        } else {
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
    pub fn try_has_key(&self, key: &[u8]) -> Result<bool> {
        self.try_dereference()?;
        let (is_dictionary, child) = self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => (true, entries.get(key).cloned()),
            _ => (false, None),
        });
        if !is_dictionary {
            self.type_warning(
                "dictionary",
                "returning false for a key containment request",
            )?;
            return Ok(false);
        }
        match child {
            Some(child) => Ok(!child.try_is_null()?),
            None => Ok(false),
        }
    }

    /// Return a child only when this handle is a non-null dictionary. qpdf's
    /// `getKeyIfDict` short-circuits null values before invoking `getKey`, so
    /// null receivers do not emit a warning (`libqpdf/QPDFObjectHandle.cc:1008-1014`).
    pub fn try_get_key_if_dict(&self, key: &[u8]) -> Result<ObjectHandle> {
        if self.try_is_null()? {
            return Ok(ObjectHandle::null());
        }
        self.try_get_key(key)
    }

    /// Return qpdf's dictionary snapshot, warning and treating a non-dict as
    /// empty (`libqpdf/QPDFObjectHandle.cc:1010-1021`).
    pub fn try_get_dict_as_map(&self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        self.try_dereference()?;
        match self.as_dictionary() {
            Some(entries) => Ok(entries),
            None => {
                self.type_warning("dictionary", "treating as empty")?;
                Ok(BTreeMap::new())
            }
        }
    }

    /// Fallible spelling of qpdf's `hasKey` accessor for callers crossing the
    /// crate boundary.
    pub fn try_get_has_key(&self, key: &[u8]) -> Result<bool> {
        self.try_has_key(key)
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
    /// is the lexicographic order of the keys, not insertion order; a repeated
    /// key keeps its last value. Values
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
                filter_on_write: true,
                stream_length: 0,
            },
            NO_PARSED_OFFSET,
        )
    }

    /// Construct a direct real value that preserves a non-canonical source
    /// literal (e.g. `.4`) alongside its parsed value, mirroring
    /// the parser's preserved-literal value, so that a real number written in
    /// the source PDF unparses byte-identically. `literal` is expected to parse
    /// back to `value` and to differ from `value`'s canonical string form.
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
    /// otherwise. The real-or-real-literal distinction is collapsed by this
    /// accessor. It never performs resolution itself.
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

    /// True if this handle's value is known to be null. An indirect handle
    /// whose value has not yet been resolved returns `false` — this method
    /// never performs resolution itself, so an unresolved handle is not
    /// assumed to be null. Once resolved, this reflects the real value:
    /// `true` for both a genuinely parsed `null` object and a reference that
    /// qpdf resolved to its null fallback. A handle disconnected when its
    /// owning document is dropped is `Destroyed`, not null.
    pub fn is_null(&self) -> bool {
        let state = self.0.borrow().state.clone();
        let is_null = matches!(&*state.borrow(), ObjectValue::Null);
        is_null
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
    /// qpdf's usual decoded name spelling with a leading `/` for
    /// parser-created and factory-created dictionaries. A raw key supplied to
    /// [`Self::replace_key`] retains its qpdf spelling, including a
    /// deliberately slashless first byte.
    pub fn as_dictionary(&self) -> Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Dictionary(entries)) => Some(entries.clone()),
            _ => None,
        })
    }

    /// Return qpdf's live child handle for `key`, or a contextual null for a
    /// missing key/non-dictionary receiver.
    ///
    /// This convenience method has the same successful behavior as qpdf's
    /// `QPDFObjectHandle::getKey` (`libqpdf/QPDFObjectHandle.cc:978-989`). It
    /// panics when lazy resolution or qpdf's type warning would return an
    /// error; callers that need to propagate that error should use
    /// [`Self::try_get_key`]. The successful behavior includes the
    /// parent-context description on a missing-key null from
    /// `QPDF_Dictionary::getKey` (`libqpdf/QPDF_Dictionary.cc:103-115`).
    ///
    /// `key` must be qpdf's decoded, canonical dictionary key including its
    /// leading `/` (for example, `/Type`); lookup is exact and slashless
    /// keys are missing.
    pub fn get_key(&self, key: &[u8]) -> ObjectHandle {
        self.try_get_key(key)
            .unwrap_or_else(|error| panic!("ObjectHandle::get_key failed: {error}"))
    }

    /// Return whether qpdf considers `key` visible in this dictionary.
    ///
    /// This convenience method panics on lazy-resolution or type-warning
    /// errors; use [`Self::try_has_key`] when the error must be propagated. A
    /// present value that resolves to null is absent, matching
    /// `QPDF_Dictionary::hasKey` (`libqpdf/QPDF_Dictionary.cc:97-101`) and
    /// `QPDFObjectHandle::hasKey` (`libqpdf/QPDFObjectHandle.cc:966-976`).
    /// `key` must be qpdf's decoded, canonical dictionary key including its
    /// leading `/`; lookup is exact and slashless keys are absent.
    pub fn has_key(&self, key: &[u8]) -> bool {
        self.try_has_key(key)
            .unwrap_or_else(|error| panic!("ObjectHandle::has_key failed: {error}"))
    }

    /// Insert or overwrite `key` in this handle's dictionary with `value`,
    /// mutating the live value every other clone of this handle also
    /// observes — mirrors `QPDFObjectHandle::replaceKey`
    /// (`libqpdf/QPDFObjectHandle.cc:1199-1209`) and
    /// `QPDF_Dictionary::replaceKey`
    /// (`libqpdf/QPDF_Dictionary.cc:135-153`). The receiver is dereferenced
    /// before its dictionary type is inspected, matching qpdf's
    /// `asDictionary()` boundary. A non-dictionary receiver emits qpdf's
    /// type warning and is otherwise ignored. Ownership is checked with
    /// qpdf's own shallow `checkOwnership` comparison
    /// (`libqpdf/QPDFObjectHandle.cc:2355-2365`) before insertion — only
    /// `self`'s and `value`'s own owning document, never a walk into
    /// `value`'s descendants — and a value indirectly owned by a different
    /// document returns qpdf's `checkOwnership` logic error as
    /// [`Error::Internal`]. A programmatic direct value (including a direct
    /// null) is unowned for this check, while a non-null direct value parsed
    /// from a file carries the parser's QPDF context. In either case,
    /// containment alone never confers ownership. A direct null removes the
    /// key, while an indirect null or dangling indirect reference is retained
    /// as the dictionary value. Also a no-op if `value` is a direct handle
    /// sharing `self`'s value state — inserting it into the dictionary would
    /// otherwise create a direct cycle that none of this crate's recursive
    /// walkers (`shallow_copy`, `materialize`, `Debug`) guard against, since
    /// they only stop recursion at an indirect-handle boundary. This does
    /// not detect a multi-hop reciprocal cycle built from two or more
    /// `replace_key` calls across distinct direct dictionaries. `key` must be
    /// qpdf's decoded, canonical dictionary key including its leading `/`; this
    /// API does not normalize slashless input.
    ///
    /// This mutates the live handle graph directly. If `self`'s ref has
    /// already been read through [`crate::Pdf::resolve`] or
    /// [`crate::Pdf::resolve`], resolution keeps the same canonical handle
    /// identity while this mutation changes the live value in place. A later
    /// canonical resolve observes that value; it does not rebuild a separate
    /// raw snapshot.
    ///
    /// This also has no path to inform the owning [`crate::Pdf`] that
    /// `self`'s value changed. After mutating a handle, call
    /// [`crate::Pdf::mark_object_handle_dirty`] with `self`. That marks the
    /// handle itself when it is an indirect object, or its containing indirect
    /// owner(s) when it is a direct child. For an already-registered indirect
    /// handle, [`crate::Pdf::mark_object_dirty`] with the same ref remains the
    /// equivalent lower-level operation.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] when the value belongs to a different
    /// document than the dictionary receiver, matching qpdf's
    /// `checkOwnership` failure boundary.
    pub fn replace_key(&self, key: &[u8], value: ObjectHandle) -> Result<()> {
        if !self.prepare_dictionary_mutation("ignoring key replacement request")? {
            return Ok(());
        }
        self.check_key_value_ownership(&value)?;
        self.replace_key_unchecked(key, value);
        Ok(())
    }

    /// Replace `key` and return the supplied value, mirroring
    /// `QPDFObjectHandle::replaceKeyAndGetNew`
    /// (`libqpdf/QPDFObjectHandle.cc:1213-1217`). The ordinary replacement
    /// operation owns resolution, warning, ownership, and live-identity
    /// behavior; this method only returns the same handle after it completes.
    pub fn replace_key_and_get_new(&self, key: &[u8], value: ObjectHandle) -> Result<ObjectHandle> {
        self.replace_key(key, value.clone())?;
        Ok(value)
    }

    /// Remove `key`, replace it with `value`, and return the old value,
    /// mirroring `QPDFObjectHandle::replaceKeyAndGetOld`
    /// (`libqpdf/QPDFObjectHandle.cc:1219-1225`). The removal is deliberately
    /// performed before the replacement, so an ownership failure in the
    /// second operation has the same partial-mutation boundary as qpdf.
    pub fn replace_key_and_get_old(&self, key: &[u8], value: ObjectHandle) -> Result<ObjectHandle> {
        let old = self.remove_key_and_get_old(key)?;
        self.replace_key(key, value)?;
        Ok(old)
    }

    /// Remove `key` and return its old value, or a fresh direct null when the
    /// key was absent, mirroring `QPDFObjectHandle::removeKeyAndGetOld`
    /// (`libqpdf/QPDFObjectHandle.cc:1240-1248`). The receiver is resolved
    /// before the old dictionary value is read. A non-dictionary receiver
    /// emits qpdf's `removeKey` type warning and returns a direct null when
    /// warning delivery succeeds.
    pub fn remove_key_and_get_old(&self, key: &[u8]) -> Result<ObjectHandle> {
        self.try_dereference()?;
        let old = self.with_value(|current| match current {
            Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
            _ => None,
        });
        if !self.with_value(|current| matches!(current, Some(ObjectValue::Dictionary(_)))) {
            self.type_warning("dictionary", "ignoring key removal request")?;
            return Ok(ObjectHandle::null());
        }
        self.remove_key(key);
        Ok(old.unwrap_or_else(ObjectHandle::null))
    }

    fn replace_key_unchecked(&self, key: &[u8], value: ObjectHandle) {
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

    /// Resolve this handle and emit qpdf's dictionary type warning when a
    /// dictionary mutation is attempted on another object type. This is the
    /// `asDictionary()` branch shared by `replaceKey` and its AndGet variant.
    fn prepare_dictionary_mutation(&self, warning: &str) -> Result<bool> {
        self.try_dereference()?;
        if self.with_value(|current| matches!(current, Some(ObjectValue::Dictionary(_)))) {
            return Ok(true);
        }
        self.type_warning("dictionary", warning)?;
        Ok(false)
    }

    /// Insert or replace `key` with `value` on a live dictionary, preserving
    /// a literal direct null rather than collapsing it to key removal.
    ///
    /// [`Self::replace_key`] mirrors qpdf's `QPDF_Dictionary::replaceKey`
    /// null/absence conflation (a direct null and a missing key are
    /// indistinguishable per the PDF spec,
    /// `libqpdf/QPDF_Dictionary.cc:136-146`) — the correct behavior for an
    /// ordinary semantic document edit. This method exists for a different
    /// responsibility: restoring a dictionary to an exact prior raw state
    /// (e.g. undoing a writer's temporary output-only mutation), where a
    /// previously captured direct-null entry must be put back as a present
    /// key with that null value, not silently dropped. Ownership is checked
    /// the same way as [`Self::replace_key`]; this is also a no-op on a
    /// non-dictionary handle for the same reason.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] when `value` belongs to a different
    /// document than the dictionary receiver, matching [`Self::replace_key`]'s
    /// ownership boundary.
    pub(crate) fn restore_key_raw(&self, key: &[u8], value: ObjectHandle) -> Result<()> {
        // cov:ignore-start: `restore_catalog_extensions` is this method's
        // only caller and always invokes it on an already-resolved Catalog
        // dictionary, so the non-dictionary guard below is defensive, not
        // reachable from that call site.
        if !self.with_value(|current| matches!(current, Some(ObjectValue::Dictionary(_)))) {
            return Ok(());
        }
        // cov:ignore-end
        self.check_key_value_ownership(&value)?;
        // cov:ignore-start: the same caller only ever restores a snapshot
        // captured from this very dictionary's own child, which can never be
        // a direct alias of the dictionary itself.
        if self.is_direct_value_alias(&value) {
            return Ok(());
        }
        // cov:ignore-end
        // cov:ignore-start: the dictionary type was already confirmed above,
        // so the closure's non-dictionary fallthrough is unreachable here.
        let replaced = self.with_value_mut(|v| {
            if let Some(ObjectValue::Dictionary(entries)) = v {
                return Some(entries.insert(key.to_vec(), value.clone()));
            }
            None
        });
        // cov:ignore-end
        if let Some(old_value) = replaced {
            if let Some(old_value) = old_value {
                self.detach_child_from_state_owners(&old_value);
            }
            self.attach_child_to_state_owners(&value);
        } // cov:ignore: closing brace has no llvm-cov region after the covered attach_child_to_state_owners call
        Ok(())
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
    /// indirect array through its parent dictionary or copy it into a separate
    /// value model.
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

    /// qpdf-facing append spelling used by consumers that need a fallible
    /// Rust boundary. It preserves the existing canonical mutation path.
    pub fn try_append_array_item(&self, value: ObjectHandle) -> Result<()> {
        self.append_array_item(value)
    }

    /// qpdf-facing vector replacement spelling used by consumers that need a
    /// fallible Rust boundary. It preserves qpdf's non-transactional prefix
    /// mutation behavior through [`Self::set_array_items`].
    pub fn try_set_array_items(&self, items: Vec<ObjectHandle>) -> Result<()> {
        self.set_array_items(items)
    }

    /// Set an array item using qpdf's signed index domain. Negative and
    /// oversized indexes produce an object warning on arrays; a non-array
    /// receiver produces the type warning and is left unchanged.
    pub fn try_set_array_item_at(&self, index: i64, value: ObjectHandle) -> Result<()> {
        self.try_dereference()?;
        let Some(index) = usize::try_from(index).ok() else {
            if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
                self.object_warning("ignoring attempt to set out of bounds array item")?;
            } else {
                self.type_warning("array", "ignoring attempt to set item")?;
            }
            return Ok(());
        };
        let in_bounds = self.with_value(
            |current| matches!(current, Some(ObjectValue::Array(items)) if index < items.len()),
        );
        if !in_bounds {
            if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
                self.object_warning("ignoring attempt to set out of bounds array item")?;
            } else {
                self.type_warning("array", "ignoring attempt to set item")?;
            }
            return Ok(());
        }
        self.set_array_item(index, value)
    }

    /// Insert an array item using qpdf's signed index domain. The valid range
    /// is inclusive of the current length.
    pub fn try_insert_array_item_at(&self, index: i64, value: ObjectHandle) -> Result<()> {
        self.try_dereference()?;
        let Some(index) = usize::try_from(index).ok() else {
            if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
                self.object_warning("ignoring attempt to insert out of bounds array item")?;
            } else {
                self.type_warning("array", "ignoring attempt to insert item")?;
            }
            return Ok(());
        };
        let in_bounds = self.with_value(
            |current| matches!(current, Some(ObjectValue::Array(items)) if index <= items.len()),
        );
        if !in_bounds {
            if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
                self.object_warning("ignoring attempt to insert out of bounds array item")?;
            } else {
                self.type_warning("array", "ignoring attempt to insert item")?;
            }
            return Ok(());
        }
        self.insert_array_item(index, value)
    }

    /// Erase an array item using qpdf's signed index domain.
    pub fn try_erase_array_item_at(&self, index: i64) -> Result<()> {
        self.try_dereference()?;
        let Some(index) = usize::try_from(index).ok() else {
            if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
                self.object_warning("ignoring attempt to erase out of bounds array item")?;
            } else {
                self.type_warning("array", "ignoring attempt to erase item")?;
            }
            return Ok(());
        };
        let in_bounds = self.with_value(
            |current| matches!(current, Some(ObjectValue::Array(items)) if index < items.len()),
        );
        if !in_bounds {
            if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
                self.object_warning("ignoring attempt to erase out of bounds array item")?;
            } else {
                self.type_warning("array", "ignoring attempt to erase item")?;
            }
            return Ok(());
        }
        self.erase_array_item(index)
    }

    /// Resolve the receiver and emit qpdf's type warning when it is not an
    /// array. This is deliberately separate from `with_value_mut`: the
    /// latter remains a no-hidden-I/O helper for legacy mutation paths, while
    /// qpdf's public array mutators call `asArray()` and therefore resolve
    /// their holder first.
    fn prepare_array_mutation(&self, warning: &str) -> Result<bool> {
        self.try_dereference()?;
        if self.with_value(|current| matches!(current, Some(ObjectValue::Array(_)))) {
            return Ok(true);
        }
        self.type_warning("array", warning)?;
        Ok(false)
    }

    /// Port `QPDF_Array::checkOwnership` (`libqpdf/QPDF_Array.cc:10-26`) at
    /// the Rust error boundary. As with
    /// [`Self::check_key_value_ownership`], compare only the receiver's and
    /// item's own active PDF identities. A programmatic direct value can
    /// retain propagated containment ids, but those ids are not qpdf ownership
    /// and must not reject an array insertion; a non-null parser-created direct
    /// value carries its source document identity just as qpdf's parsed value
    /// does.
    fn check_array_item_ownership(&self, item: &ObjectHandle) -> Result<()> {
        let item_is_destroyed = {
            let slot = item.0.borrow();
            let state = slot.state.borrow();
            let destroyed = matches!(&*state, ObjectValue::Destroyed);
            destroyed
        };
        // qpdf's QPDF_Array::checkOwnership rejects an item whose
        // getObjectPtr() is null -- the default-constructed
        // QPDFObjectHandle() shape, distinct from an initialized handle
        // whatever its current value type -- with this same message
        // (`libqpdf/QPDF_Array.cc:9-24`).
        if item_is_destroyed || !item.is_initialized() {
            return Err(Error::Internal(
                "Attempting to add an uninitialized object to a QPDF_Array.".to_owned(),
            ));
        }

        self.check_key_value_ownership(item)
    }

    /// Port `QPDFObjectHandle::checkOwnership` (`libqpdf/QPDFObjectHandle.cc:
    /// 2355-2365`) exactly: qpdf compares only `this->getOwningQPDF()` and
    /// `item.getOwningQPDF()`, two O(1) reads of each handle's *own* owning
    /// document -- never a walk into either handle's descendants. A value's
    /// `getOwningQPDF()` in qpdf is `nullptr` for programmatically created
    /// direct values and literal nulls; file-parser-created direct values do
    /// carry the parser's QPDF context (`QPDFParser.cc:394-444`). Mere
    /// containment inside another document's object graph does not confer
    /// ownership. This deliberately does not consult
    /// [`Self::belongs_exclusively_to_pdf`] or the
    /// `pdf_unique_ids` live-containment set that field reads from: that
    /// bookkeeping tracks *current* containment for dirty-marking
    /// ([`Self::containing_object_refs_for_pdf`]) and keeps a value's prior
    /// document id after it is no longer reachable there, which is not
    /// qpdf's ownership semantics and would reject a direct value (a null
    /// or any other scalar) that merely passed through a different
    /// document's object graph at some earlier point, even though qpdf
    /// itself never associates ownership with a direct value that way.
    fn check_key_value_ownership(&self, value: &ObjectHandle) -> Result<()> {
        if let (Some(self_pdf_unique_id), Some(value_pdf_unique_id)) =
            (self.owning_pdf_unique_id(), value.owning_pdf_unique_id())
        {
            if self_pdf_unique_id != value_pdf_unique_id {
                return Err(Error::Internal(FOREIGN_OBJECT_OWNERSHIP_ERROR.to_owned()));
            }
        }
        Ok(())
    }

    /// Replace an existing array item with `value`, preserving `value`'s
    /// shared handle identity. Returns `false` when this handle is not an
    /// array or `index` is out of bounds.
    #[cfg(test)]
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
    #[cfg(test)]
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
            let children = Self::direct_children(&state.borrow());
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

    /// Empty a final owner's direct container edges before its `ObjectHandle`
    /// or `ObjectHandleIdentity` wrapper releases the slot.
    ///
    /// qpdf's `QPDFObject`/`QPDFValue` ownership is shared, so a child may be
    /// removed here only when both the slot and its payload have no other
    /// strong owner. Leaving an empty container in the payload makes the
    /// ordinary field drop constant-depth; the moved children are then walked
    /// by [`Self::drain_owned_descendants`].
    fn take_owned_direct_children(slot: &Rc<RefCell<ObjectSlot>>) -> Vec<Self> {
        if Rc::strong_count(slot) != 1 {
            return Vec::new();
        }

        let parent = Rc::downgrade(slot);
        let children = {
            let slot_ref = slot.borrow();
            if Rc::strong_count(&slot_ref.state) != 1 {
                return Vec::new();
            }

            let mut state = slot_ref.state.borrow_mut();
            match &mut *state {
                ObjectValue::Array(children) => std::mem::take(children),
                ObjectValue::Dictionary(entries) => std::mem::take(entries).into_values().collect(),
                ObjectValue::Stream { stream_dict, .. } => {
                    vec![std::mem::replace(stream_dict, ObjectHandle::null())]
                }
                _ => Vec::new(),
            }
        };

        for child in &children {
            Self::detach_child_from_parent(child, &parent);
        }
        children
    }

    /// Release a direct acyclic object graph with an explicit heap worklist.
    ///
    /// This is deliberately called from both wrapper types that retain the
    /// slot's `Rc`: internal identity keys can outlive the last public
    /// `ObjectHandle`, and must not reintroduce recursive field destruction.
    fn drain_owned_descendants(slot: &Rc<RefCell<ObjectSlot>>) {
        let mut pending = Self::take_owned_direct_children(slot);
        while let Some(handle) = pending.pop() {
            pending.extend(Self::take_owned_direct_children(&handle.0));
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
                let children = Self::direct_children(&state.borrow());
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
    /// indirect handle is unresolved/destroyed. `key` must be qpdf's
    /// decoded, canonical dictionary key including its leading `/`; this API
    /// does not normalize slashless input. Never performs resolution itself.
    ///
    /// See [`Self::replace_key`]'s doc comment for the same canonical
    /// resolution behavior and the
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
    /// resolution itself: shallow-copying an unresolved/destroyed
    /// indirect handle produces a direct null handle, matching every other
    /// accessor's "no hidden I/O" rule.
    ///
    /// A reserved handle (see [`crate::Pdf::new_reserved`]) is the one
    /// exception to that null fallback: `QPDF_Reserved::copy(bool shallow)`
    /// (`libqpdf/QPDF_Reserved.cc:14-19`) ignores its `shallow` argument and
    /// unconditionally returns `create()`, a brand-new `QPDF_Reserved`
    /// instance, never null and never a throw — resolving a reserved
    /// handle's state costs no I/O, so the "no hidden I/O" rationale above
    /// does not apply to it. This method mirrors that: a fresh, direct,
    /// independent reserved sentinel, sharing no identity with `self`.
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
        if self.is_reserved() {
            return Ok(ObjectHandle::new_reserved_direct());
        }
        stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
            self.with_value(|value| match value {
                Some(v) => Ok(ObjectHandle::new_direct_preserving_dictionary_keys(
                    shallow_copy_value(v)?,
                    NO_PARSED_OFFSET,
                )),
                None => Ok(ObjectHandle::null()),
            })
        })
    }

    /// Convert this handle to a direct copy of its reachable object graph,
    /// mirroring qpdf's `QPDFObjectHandle::makeDirect`
    /// (`libqpdf/QPDFObjectHandle.cc:2091-2133,2154-2157`). The receiver is
    /// rebound to the new handle; aliases retained before this call continue
    /// to observe the original object, just as qpdf's assignment to the
    /// receiver's `shared_ptr<QPDFObject>` does.
    ///
    /// Arrays and dictionaries are copied recursively through every indirect
    /// boundary. Each occurrence is copied independently, so two references
    /// that used to identify the same indirect object no longer alias in the
    /// resulting direct graph. A per-call identity set detects an indirect
    /// cycle while it is being traversed.
    ///
    /// When `allow_streams` is true, stream handles are retained as-is and
    /// are not converted to direct values. When it is false, encountering a
    /// stream returns qpdf's exact runtime-error text.
    pub fn make_direct(&mut self, allow_streams: bool) -> Result<()> {
        #[allow(
            clippy::mutable_key_type,
            reason = "identity key compares only Rc pointer identity and retains the slot deliberately"
        )]
        let mut visited = std::collections::HashSet::new();
        let replacement =
            stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
                self.make_direct_copy(&mut visited, allow_streams)
            })?;
        *self = replacement;
        Ok(())
    }

    #[allow(
        clippy::mutable_key_type,
        reason = "identity key compares only Rc pointer identity and retains the slot deliberately"
    )]
    fn make_direct_copy(
        &self,
        visited: &mut std::collections::HashSet<ObjectHandleIdentity>,
        allow_streams: bool,
    ) -> Result<Self> {
        self.try_dereference()?;
        let identity = self.identity_key();
        if !visited.insert(identity.clone()) {
            return Err(Error::System(
                "loop detected while converting object from indirect to direct".to_owned(),
            ));
        }

        let result = self.make_direct_copy_value(visited, allow_streams);
        visited.remove(&identity);
        result
    }

    #[allow(
        clippy::mutable_key_type,
        reason = "identity key compares only Rc pointer identity and retains the slot deliberately"
    )]
    fn make_direct_copy_value(
        &self,
        visited: &mut std::collections::HashSet<ObjectHandleIdentity>,
        allow_streams: bool,
    ) -> Result<Self> {
        // Snapshot the value before descending. Resolving a child may re-enter
        // the same document resolver, so no RefCell borrow may span the
        // recursive call.
        let value = self
            .with_value(|value| value.cloned())
            .expect("resolved ObjectHandle always exposes its value");

        match value {
            ObjectValue::Boolean(_)
            | ObjectValue::Integer(_)
            | ObjectValue::Name(_)
            | ObjectValue::Null
            | ObjectValue::Real(_)
            | ObjectValue::RealLiteral { .. }
            | ObjectValue::String(_) => Ok(Self::from_value(value)),
            ObjectValue::Array(items) => {
                let mut copied = Vec::with_capacity(items.len());
                for item in items {
                    copied.push(item.make_direct_copy(visited, allow_streams)?);
                }
                Ok(Self::new_direct(
                    ObjectValue::Array(copied),
                    NO_PARSED_OFFSET,
                ))
            }
            ObjectValue::Dictionary(entries) => {
                let mut copied = std::collections::BTreeMap::new();
                for (key, item) in entries {
                    copied.insert(key, item.make_direct_copy(visited, allow_streams)?);
                }
                Ok(Self::new_direct(
                    ObjectValue::Dictionary(copied),
                    NO_PARSED_OFFSET,
                ))
            }
            ObjectValue::Stream { .. } => {
                if allow_streams {
                    Ok(self.clone())
                } else {
                    Err(Error::System(
                        "attempt to make a stream into a direct object".to_owned(),
                    ))
                }
            }
            ObjectValue::Reserved => Err(Error::System(
                "QPDFObjectHandle: attempting to make a reserved object handle direct".to_owned(),
            )),
            ObjectValue::Unresolved
            | ObjectValue::Destroyed
            | ObjectValue::Operator(_)
            | ObjectValue::InlineImage(_) => Err(Error::System(
                "QPDFObjectHandle::makeDirectInternal: unknown object type".to_owned(),
            )),
        }
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
        let type_name = self.type_name()?;
        let Some(source_dict) = self.as_stream_dict() else {
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                type_name
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
            destination_dict.replace_key_unchecked(&key, value);
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
    /// # Preconditions
    ///
    /// The receiver, `other`, and the top-level resource-category handles
    /// still use this port's intentionally non-fallible shape accessors, so
    /// callers must resolve those handles before calling this method. This
    /// preserves the existing precondition for `as_dictionary` and `as_array`,
    /// which do not perform hidden I/O and determine the outer merge shape.
    /// Dictionary key lookup inside a confirmed category uses the fallible
    /// qpdf-shaped accessors and propagates resolution errors. Nested type
    /// inspection follows qpdf: `isScalar()` resolves
    /// every array item (`QPDFObjectHandle.cc:449-452`, with the scalar
    /// accessors following the same `dereference() && ...` pattern, e.g.
    /// `isBool` at `:338-341`), and `getResourceNames()` resolves each
    /// second-level value through `isDictionary()` (`:1156-1170,431-434`).
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
    /// See [`Self::replace_key`]'s doc comment for the same canonical
    /// resolution behavior and the
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
    /// Also propagates lazy-resolution errors from nested array items and
    /// second-level dictionary values inspected by the qpdf-shaped helpers.
    pub fn merge_resources(
        &self,
        other: &ObjectHandle,
        mut conflicts: Option<&mut ResourceConflicts>,
    ) -> Result<()> {
        let (Some(_), Some(other_entries)) = (self.as_dictionary(), other.as_dictionary()) else {
            return Ok(());
        };
        for (rtype, other_val) in other_entries {
            if !self.try_has_key(&rtype)? {
                self.replace_key(&rtype, other_val.shallow_copy()?)?;
                continue;
            }
            let mut this_val = self.try_get_key(&rtype)?;
            if this_val.as_dictionary().is_some() && other_val.as_dictionary().is_some() {
                if this_val.is_indirect() {
                    let privatized = this_val.shallow_copy()?;
                    self.replace_key(&rtype, privatized.clone())?;
                    this_val = privatized;
                }
                merge_resource_subdict(&this_val, &other_val, &rtype, conflicts.as_deref_mut())?;
            } else if this_val.as_array().is_some() && other_val.as_array().is_some() {
                merge_resource_array(&this_val, &other_val)?;
            }
            // Any other shape combination for an existing rtype: untouched,
            // matching qpdf's own fallthrough (neither the dictionary nor
            // the array arm matches, and there is no further branch).
        }
        Ok(())
    }

    /// Make every direct value in every dictionary-valued top-level resource
    /// category indirect, mirroring `QPDFObjectHandle::makeResourcesIndirect`
    /// (`libqpdf/QPDFObjectHandle.cc:1042-1060`). The category dictionaries
    /// themselves are not promoted and the walk does not recurse into a
    /// promoted value. qpdf registers the existing `QPDFObject` allocation
    /// with `QPDF::makeIndirectObject` (`libqpdf/QPDF.cc:1882-1894`), so the
    /// Rust path uses the canonical in-place promotion primitive rather than a
    /// shallow or materialized clone.
    ///
    /// `owning_pdf` is mutable because promotion updates its canonical object
    /// registry and the live dictionary mutation must be reported to its
    /// writer dirty-set. This corresponds to qpdf's
    /// `init_dr_map` call order, which performs this normalization before
    /// `mergeResources` (`libqpdf/QPDFAcroFormDocumentHelper.cc:775-800`).
    pub fn make_resources_indirect<R: std::io::Read + std::io::Seek + 'static>(
        &self,
        owning_pdf: &mut crate::Pdf<R>,
    ) -> Result<()> {
        let Some(top_entries) = self.try_as_dictionary()? else {
            return Ok(());
        };

        for (_resource_type, category) in top_entries {
            let Some(category_entries) = category.try_as_dictionary()? else {
                continue;
            };
            let mut changed = false;
            for (name, value) in category_entries {
                if value.is_indirect() {
                    continue;
                }
                let indirect = owning_pdf.make_indirect_from_object_handle(value)?;
                category.replace_key(&name, indirect)?;
                changed = true;
            }
            if changed {
                let dirty_result = owning_pdf.mark_object_handle_dirty(&category);
                dirty_result?; // cov:ignore: successful ? continuation has no llvm-cov region; call is covered on the prior line
            } // cov:ignore: branch closing brace has no llvm-cov region after successful ? continuation
        }
        Ok(())
    }

    /// Return all names in the second-level dictionaries of a resource
    /// dictionary.
    ///
    /// This ports `QPDFObjectHandle::getResourceNames`
    /// (`include/qpdf/QPDFObjectHandle.hh:831-835`,
    /// `libqpdf/QPDFObjectHandle.cc:1156-1170`). The receiver and each
    /// top-level value are resolved through the owning document before their
    /// dictionary shape is inspected. The fallible `Result` preserves this
    /// crate's resolver error boundary; qpdf's equivalent reports the same
    /// collection for successfully resolved values.
    pub fn get_resource_names(&self) -> Result<std::collections::BTreeSet<Vec<u8>>> {
        try_get_resource_names(self)
    }

    /// Return the unique resource name qpdf would select for `prefix`.
    ///
    /// This ports `QPDFObjectHandle::getUniqueResourceName`
    /// (`libqpdf/QPDFObjectHandle.cc:1175-1192`). When `resource_names` is
    /// absent, the names are collected from the second-level dictionary keys
    /// exactly as the private `try_get_resource_names` helper does. `min_suffix` is left at
    /// the suffix that was selected, rather than advanced past it, so callers
    /// can reuse the cursor for a later insertion. The `usize`/byte-vector
    /// representation is the Rust spelling of qpdf's `int`/`std::string`
    /// boundary; no PDF bytes are materialized while searching.
    pub fn get_unique_resource_name(
        &self,
        prefix: &[u8],
        min_suffix: &mut usize,
        resource_names: Option<&std::collections::BTreeSet<Vec<u8>>>,
    ) -> Result<Vec<u8>> {
        let names = match resource_names {
            Some(names) => names.clone(),
            None => self.get_resource_names()?,
        };
        let max_suffix = *min_suffix + names.len();
        while *min_suffix <= max_suffix {
            let mut candidate = prefix.to_vec();
            candidate.extend(min_suffix.to_string().into_bytes());
            if !names.contains(&candidate) {
                return Ok(candidate);
            }
            *min_suffix += 1;
        }
        // qpdf treats this as an internal coding error and throws
        // std::logic_error (`libqpdf/QPDFObjectHandle.cc:1188-1191`); this
        // maps to `Error::Internal` like every other logic_error in this
        // file. The loop tests one more candidate than there are entries in
        // `names`, so by pigeonhole this is unreachable for any input.
        // cov:ignore-start: unreachable by the pigeonhole argument above for any input
        Err(Error::Internal(
            "unable to find unconflicting resource name".to_owned(),
        ))
        // cov:ignore-end
    }

    /// Whether this handle is a Form XObject.
    ///
    /// This ports `QPDFObjectHandle::isFormXObject`
    /// (`libqpdf/QPDFObjectHandle.cc:2340-2343`). The stream and subtype
    /// holder are resolved through the owning document before inspection;
    /// programmatically constructed direct handles remain context-free.
    pub fn is_form_xobject(&self) -> Result<bool> {
        if self.type_code()? != 10 {
            return Ok(false);
        }
        let Some(stream_dict) = self.as_stream_dict() else {
            return Ok(false); // cov:ignore: stream values always carry a dictionary
        };
        stream_dict
            .try_get_key(b"/Subtype")?
            .try_is_name_and_equals(b"Form")
    }

    /// Whether this handle is an Image XObject.
    ///
    /// This ports `QPDFObjectHandle::isImage`
    /// (`libqpdf/QPDFObjectHandle.cc:2345-2352`). With
    /// `exclude_imagemask` set, a boolean `/ImageMask true` excludes the
    /// stream; non-boolean or missing `/ImageMask` values do not.
    pub fn is_image(&self, exclude_imagemask: bool) -> Result<bool> {
        if self.type_code()? != 10 {
            return Ok(false);
        }
        let Some(stream_dict) = self.as_stream_dict() else {
            return Ok(false); // cov:ignore: stream values always carry a dictionary
        };
        if !stream_dict
            .try_get_key(b"/Subtype")?
            .try_is_name_and_equals(b"Image")?
        {
            return Ok(false);
        }
        if !exclude_imagemask {
            return Ok(true);
        }
        let image_mask = stream_dict.try_get_key(b"/ImageMask")?;
        image_mask.try_dereference()?;
        Ok(image_mask.as_boolean() != Some(true))
    }

    /// Return the page's `/Contents` as the qpdf-normalized stream list.
    ///
    /// This ports `QPDFObjectHandle::getPageContents` and its
    /// `arrayOrStreamToStreamArray` helper
    /// (`libqpdf/QPDFObjectHandle.cc:1438-1493`). A missing or null `/Contents`
    /// is an empty list. A single stream is returned as a one-item list. An
    /// array resolves each member, keeps stream members in order, and reports
    /// non-stream members at the same warning boundary as qpdf. The returned
    /// handles retain their canonical identity and no stream data is decoded.
    pub fn get_page_contents(&self) -> Result<Vec<ObjectHandle>> {
        Ok(self.page_contents_with_description()?.0)
    }

    /// Add a content stream to this page, preserving qpdf's always-array
    /// result and prepend/append order.
    ///
    /// This ports `QPDFObjectHandle::addPageContents`
    /// (`libqpdf/QPDFObjectHandle.cc:1495-1513`). The incoming handle is
    /// checked before the existing page contents are inspected, matching
    /// qpdf's `assertStream` ordering. Existing malformed content arrays use
    /// the same normalization and warning boundary as [`Self::get_page_contents`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] when `new_contents` is not a stream, and
    /// propagates content normalization, ownership, or replacement failures.
    pub fn add_page_contents(&self, new_contents: ObjectHandle, first: bool) -> Result<()> {
        if new_contents.type_code()? != 10 {
            let type_name = new_contents.type_name()?;
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                type_name
            )));
        }

        let mut content_streams = Vec::new();
        if first {
            content_streams.push(new_contents.clone());
        }
        content_streams.extend(self.get_page_contents()?);
        if !first {
            content_streams.push(new_contents);
        }
        self.replace_key(b"/Contents", ObjectHandle::array(content_streams))
    }

    /// Saturate `value` to the `i32` range, warning on `self` (the source of
    /// the value) at each clamp.
    ///
    /// Ports the saturating half of `QPDFObjectHandle::getIntValueAsInt`
    /// (`libqpdf/QPDFObjectHandle.cc:525-543`) for a caller that has already
    /// obtained an `i64` some other way than [`Self::try_get_int_value`] --
    /// [`rotate_page`](Self::rotate_page)'s relative-rotation walk needs the
    /// same saturate-and-warn behavior qpdf's `getValueAsInt(int&)` applies
    /// to a *found* integer, without that function's unconditional warn-and-
    /// default-to-0 for a non-integer receiver, which would misfire on the
    /// common case of an ancestor with no `/Rotate` at all.
    fn saturate_i64_to_i32_range_with_warning(&self, value: i64) -> Result<i64> {
        if value < i64::from(i32::MIN) {
            self.warn_if_possible("requested value of integer is too small; returning INT_MIN")?;
            Ok(i64::from(i32::MIN))
        } else if value > i64::from(i32::MAX) {
            self.warn_if_possible("requested value of integer is too big; returning INT_MAX")?;
            Ok(i64::from(i32::MAX))
        } else {
            Ok(value)
        }
    }

    /// Set this page's rotation, optionally adding the nearest inherited
    /// `/Rotate` value.
    ///
    /// This ports `QPDFObjectHandle::rotatePage`
    /// (`libqpdf/QPDFObjectHandle.cc:1517-1546`). Relative lookup walks
    /// `/Parent` through dictionary handles, stops at the first integer
    /// `/Rotate`, ignores a non-quarter-turn inherited value, and uses
    /// canonical object identity to break cycles.
    ///
    /// # Errors
    ///
    /// Returns [`Error::System`] for a non-multiple-of-90 angle and
    /// propagates lazy-resolution, warning, or dictionary replacement errors.
    pub fn rotate_page(&self, angle: i32, relative: bool) -> Result<()> {
        if angle % 90 != 0 {
            return Err(Error::System(
                "QPDF::rotatePage called with an angle that is not a multiple of 90".to_owned(),
            ));
        }

        let mut new_angle = i64::from(angle);
        if relative {
            let mut old_angle = 0_i64;
            let mut current = self.clone();
            // qpdf's visited set is keyed by canonical object identity. The
            // retained Rc is intentional: it keeps pointer identity stable
            // for the duration of the parent walk.
            #[allow(
                clippy::mutable_key_type,
                reason = "identity key compares only Rc pointer identity and retains the slot deliberately"
            )]
            let mut visited = std::collections::HashSet::new();
            while visited.insert(current.identity_key()) {
                let rotate = current.try_get_key(b"/Rotate")?;
                if let Some(value) = rotate.try_as_integer()? {
                    // qpdf's rotatePage reads the found value through
                    // getValueAsInt(int&), which saturates out-of-i32-range
                    // integers to INT_MIN/INT_MAX with a warning
                    // (getIntValueAsInt, QPDFObjectHandle.cc:525-543) rather
                    // than using the raw value. Absent/non-integer /Rotate
                    // still falls through to the /Parent walk below, exactly
                    // as qpdf's isInteger() guard does, so this only clamps
                    // once an integer is actually found.
                    old_angle = rotate.saturate_i64_to_i32_range_with_warning(value)?;
                    break;
                }

                let parent = current.try_get_key(b"/Parent")?;
                parent.try_dereference()?;
                if parent.as_dictionary().is_some() {
                    current = parent;
                } else {
                    break;
                }
            }
            if old_angle % 90 != 0 {
                old_angle = 0;
            }
            new_angle += old_angle;
        }

        // Keep C++'s remainder semantics for negative angles while widening
        // before the addition so the Rust implementation cannot overflow at
        // the edge of the i32 API domain.
        let normalized = (new_angle + 360) % 360;
        self.replace_key(b"/Rotate", ObjectHandle::integer(normalized))
    }

    /// Replace an array-valued `/Contents` entry with one lazy stream whose
    /// bytes are produced by the canonical page-content pipeline.
    ///
    /// This ports `QPDFObjectHandle::coalesceContentStreams`
    /// (`libqpdf/QPDFObjectHandle.cc:1550-1572`). A single stream, missing
    /// `/Contents`, null, and other non-array values are no-ops. An array
    /// requires an owning document because qpdf creates a document-owned
    /// replacement stream and registers a deferred provider; no decoded byte
    /// cache is installed here.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Internal`] when an array needs coalescing but this
    /// handle has no owning document, and propagates resolution, ownership,
    /// stream allocation, or provider-registration failures.
    pub fn coalesce_content_streams(&self) -> Result<()> {
        let old_contents = self.try_get_key(b"/Contents")?;
        if old_contents.type_code()? == 10 || old_contents.as_array().is_none() {
            return Ok(());
        }

        let resolver = self.context().ok_or_else(|| {
            Error::System(
                "coalesceContentStreams called on object  with no associated PDF file".to_owned(),
            )
        })?;
        let new_contents = resolver.new_stream()?;
        self.replace_key(b"/Contents", new_contents.clone())?;
        let provider = Rc::new(CoalesceContentProvider {
            containing_page: self.clone(),
            old_contents,
        });
        new_contents.replace_stream_data_provider(
            provider,
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        )
    }

    /// Pipe this page's `/Contents` through qpdf's decoded, newline-joining
    /// content-stream pipeline.
    ///
    /// This ports `QPDFObjectHandle::pipePageContents`
    /// (`libqpdf/QPDFObjectHandle.cc:1702-1707`). `all_description` is kept
    /// internal to the delegated call so parse/stream errors retain the same
    /// page and object-generation context as qpdf.
    pub fn pipe_page_contents(&self, pipeline: &mut dyn Pipeline) -> Result<()> {
        let description = format!("page object {}", object_generation_description(self));
        let contents = self.try_get_key(b"/Contents")?;
        let mut all_description = String::new();
        contents.pipe_content_streams(pipeline, &description, &mut all_description)
    }

    /// Pipe this handle when it is a stream or an array of streams, decoding
    /// each stream at qpdf's specialized level and inserting one newline only
    /// when the preceding decoded stream did not end in one.
    ///
    /// This ports `QPDFObjectHandle::pipeContentStreams`
    /// (`libqpdf/QPDFObjectHandle.cc:1709-1737`). A private buffer mirrors
    /// qpdf's `Pl_Buffer`: stream pipelines may finish it after each member,
    /// while the caller's pipeline is written and finished exactly once after
    /// all members have been decoded.
    pub fn pipe_content_streams(
        &self,
        pipeline: &mut dyn Pipeline,
        description: &str,
        all_description: &mut String,
    ) -> Result<()> {
        let (streams, generated_description) = self.array_or_stream_to_stream_array(description)?;
        all_description.clear();
        all_description.push_str(&generated_description);

        let mut buffer = Buffer::new("concatenated content stream buffer", None);
        let mut need_newline = false;
        for stream in streams {
            if need_newline {
                buffer.write(b"\n")?;
            }

            let mut last_char = Count::new("last character", &mut buffer);
            let mut filtering_attempted = false;
            let succeeded = stream.pipe_stream_data(
                &mut last_char,
                &mut filtering_attempted,
                0,
                DecodeLevel::Specialized,
                false,
                false,
            )?;
            if !succeeded {
                return Err(Error::Unsupported(format!(
                    "content stream object {}: errors while decoding content stream",
                    object_generation_description(&stream)
                )));
            }
            last_char.finish()?;
            need_newline = last_char.last_byte() != b'\n';
        }

        let data = buffer.take_buffer()?;
        pipeline.write(&data)?;
        pipeline.finish()?;
        Ok(())
    }

    /// Parse this page's decoded `/Contents` through qpdf's ObjectHandle
    /// callback boundary.
    ///
    /// This ports `QPDFObjectHandle::parsePageContents`
    /// (`libqpdf/QPDFObjectHandle.cc:1740-1744`). The decoded bytes are
    /// buffered only for the parser, just as qpdf's `Pl_Buffer` path is; the
    /// page-content source remains the canonical stream/provider pipeline.
    pub fn parse_page_contents<C: ObjectHandleParserCallbacks>(
        &self,
        callbacks: &mut C,
    ) -> Result<()> {
        let contents = self.try_get_key(b"/Contents")?;
        self.parse_content_stream_handles(
            &contents,
            format!("page object {}", object_generation_description(self)),
            callbacks,
        )
    }

    /// Parse this stream or stream array as content, matching qpdf's
    /// `QPDFObjectHandle::parseAsContents` (`:1747-1751`).
    pub fn parse_as_contents<C: ObjectHandleParserCallbacks>(
        &self,
        callbacks: &mut C,
    ) -> Result<()> {
        self.parse_content_stream_handles(
            self,
            format!("object {}", object_generation_description(self)),
            callbacks,
        )
    }

    fn parse_content_stream_handles<C: ObjectHandleParserCallbacks>(
        &self,
        contents: &ObjectHandle,
        description: String,
        callbacks: &mut C,
    ) -> Result<()> {
        let mut buffer = Buffer::new("concatenated content stream buffer", None);
        let mut all_description = String::new();
        contents.pipe_content_streams(&mut buffer, &description, &mut all_description)?;
        let data = buffer.take_buffer()?;
        parse_content_stream_handles(
            &data,
            contents.context().or_else(|| self.context()),
            callbacks,
        )
    }

    /// Apply one qpdf lexical token filter to decoded page contents.
    ///
    /// `next` is the optional downstream pipeline corresponding to qpdf's
    /// nullable `Pipeline*` argument. The canonical page-content route owns
    /// tokenizer construction and finishes it exactly once.
    pub fn filter_page_contents<'a>(
        &self,
        filter: &'a mut dyn TokenFilter,
        next: Option<&'a mut dyn Pipeline>,
    ) -> Result<()> {
        let description = format!(
            "token filter for page object {}",
            object_generation_description(self)
        );
        let mut token_pipeline = QpdfTokenizer::new(description, filter, next);
        self.pipe_page_contents(&mut token_pipeline)
    }

    /// Apply one qpdf lexical token filter to this stream/Form contents.
    ///
    /// This ports `QPDFObjectHandle::filterAsContents`
    /// (`libqpdf/QPDFObjectHandle.cc:1762-1767`) over the specialized decode
    /// path, without introducing a second tokenizer or filter implementation.
    pub fn filter_as_contents<'a>(
        &self,
        filter: &'a mut dyn TokenFilter,
        next: Option<&'a mut dyn Pipeline>,
    ) -> Result<()> {
        let description = format!(
            "token filter for object {}",
            object_generation_description(self)
        );
        let mut token_pipeline = QpdfTokenizer::new(description, filter, next);
        let mut filtering_attempted = false;
        let success = self.pipe_stream_data(
            &mut token_pipeline,
            &mut filtering_attempted,
            0,
            DecodeLevel::Specialized,
            false,
            false,
        );
        let success = success?;
        if success {
            Ok(())
        } else {
            Err(Error::Unsupported(format!(
                "object {}: errors while decoding content stream",
                object_generation_description(self)
            )))
        }
    }

    /// The normalized page content streams plus the qpdf description used by
    /// downstream pipe/parse error messages. Keeping this alongside
    /// [`Self::get_page_contents`] prevents later entry points from
    /// reimplementing array/null handling.
    fn page_contents_with_description(&self) -> Result<(Vec<ObjectHandle>, String)> {
        let description = format!("page object {}", object_generation_description(self));
        let contents = self.try_get_key(b"/Contents")?;
        contents.array_or_stream_to_stream_array(&description)
    }

    /// Normalize this handle when it is a stream or an array of streams.
    ///
    /// The helper is intentionally private to the ObjectHandle content family;
    /// callers should use [`Self::get_page_contents`] so the description is
    /// attached to the page object rather than guessed at each call site.
    fn array_or_stream_to_stream_array(
        &self,
        description: &str,
    ) -> Result<(Vec<ObjectHandle>, String)> {
        self.try_dereference()?;
        let mut result = Vec::new();
        if let Some(items) = self.as_array() {
            for (index, item) in items.into_iter().enumerate() {
                if item.type_code()? == 10 {
                    result.push(item);
                } else {
                    item.warn_through_context(format!(
                        "{description}: item index {index} (from 0): ignoring non-stream in an array of streams"
                    ))?;
                }
            }
        } else if self.type_code()? == 10 {
            result.push(self.clone());
        } else if !self.is_null() {
            self.warn_through_context(format!(
                "{description}:  object is supposed to be a stream or an array of streams but is neither"
            ))?;
        }

        let mut all_description = description.to_owned();
        let mut first = true;
        for item in &result {
            if first {
                first = false;
            } else {
                all_description.push(',');
            }
            all_description.push_str(" stream ");
            all_description.push_str(&object_generation_description(item));
        }
        Ok((result, all_description))
    }

    /// Replace this handle's stream data with the given buffer, and — when
    /// given — its `/Filter` and `/DecodeParms` dictionary keys, mirroring
    /// `QPDFObjectHandle::replaceStreamData`'s buffer overload
    /// (`libqpdf/QPDFObjectHandle.cc:1345-1350`, delegating to
    /// `QPDF_Stream::replaceStreamData`/`replaceFilterData`,
    /// `libqpdf/QPDF_Stream.cc:637-649,669-685`). `filter`/`decode_parms`
    /// are `Some` exactly where qpdf's own overload checks
    /// `QPDFObjectHandle::isInitialized()`: `Some` installs the key through
    /// qpdf's stream-internal dictionary replacement path, `None` leaves it
    /// untouched rather than removing it. A zero byte length removes
    /// `/Length`; a nonzero length installs the exact integer, matching qpdf's
    /// shared `QPDF_Stream::replaceFilterData` boundary for buffer and
    /// provider replacement. A no-op if this handle's value is not a stream.
    ///
    /// `data` is installed as given, not copied — qpdf's own
    /// `std::shared_ptr<Buffer>` overload is documented against its
    /// string overload precisely on that point
    /// (`include/qpdf/QPDFObjectHandle.hh:1086-1097`). This is what lets one
    /// buffer back two streams, as `QPDF::copyStreamData` does
    /// (`libqpdf/QPDF.cc:2240,2256-2258`).
    ///
    /// See [`Self::replace_key`]'s doc comment for the same canonical
    /// resolution behavior and the
    /// [`crate::Pdf::mark_object_dirty`] requirement — both apply here too,
    /// since this method mutates the stream data in place and updates its
    /// dictionary through qpdf's lower-level stream-internal path.
    pub fn replace_stream_data(
        &self,
        data: Rc<Vec<u8>>,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) {
        self.set_content_normalization_applied(false);
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
    /// runtime classification as [`Error::System`]. Provider registration is
    /// restricted to indirect streams because the provider callback requires
    /// the stream's stable `ObjectRef`; a direct stream is rejected here at
    /// registration time rather than being accepted and failing later at the
    /// pipe boundary.
    ///
    /// This mutates the live stream and dictionary in place. As with
    /// [`Self::replace_stream_data`] and [`Self::replace_key`], callers that
    /// mutate a document-owned handle must call
    /// [`crate::Pdf::mark_object_handle_dirty`] (or the corresponding
    /// [`crate::Pdf::mark_object_dirty`]) before writing the document.
    pub fn replace_stream_data_provider(
        &self,
        provider: Rc<dyn StreamDataProvider>,
        filter: Option<ObjectHandle>,
        decode_parms: Option<ObjectHandle>,
    ) -> Result<()> {
        self.try_dereference()?;
        if !self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. }))) {
            let type_name = self.type_name()?;
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                type_name
            )));
        }
        self.set_content_normalization_applied(false);
        if self.object_ref().is_none() {
            return Err(Error::System(
                STREAM_DATA_PROVIDER_REQUIRES_INDIRECT_ERROR.to_owned(),
            ));
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

    /// Replace a stream's dictionary while retaining the stream object's
    /// identity and payload/provider.
    ///
    /// This is qpdf's `QPDF_Stream::replaceDict` boundary
    /// (`libqpdf/QPDF_Stream.cc:688-693`), used by
    /// `QPDF::JSONReactor::dictionaryItem` for `stream.dict`
    /// (`libqpdf/QPDF_json.cc:629-637`). The replacement is attached through
    /// the same containment bookkeeping as ordinary dictionary mutations; the
    /// caller owns the document dirty-mark decision.
    pub(crate) fn replace_stream_dict(&self, dictionary: ObjectHandle) -> Result<()> {
        self.try_dereference()?;
        dictionary.try_dereference()?;
        if dictionary.as_dictionary().is_none() {
            return Err(Error::System(
                "operation for stream dictionary attempted with a non-dictionary".to_owned(),
            ));
        }
        self.check_key_value_ownership(&dictionary)?;

        let previous = self.with_value_mut(|value| match value {
            Some(ObjectValue::Stream { stream_dict, .. }) => {
                Some(std::mem::replace(stream_dict, dictionary.clone()))
            }
            _ => None,
        });
        let Some(previous) = previous else {
            let type_name = self.type_name()?;
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                type_name
            )));
        };
        self.detach_child_from_state_owners(&previous);
        self.attach_child_to_state_owners(&dictionary);
        Ok(())
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
            dict.replace_key_unchecked(b"/Filter", filter);
        }
        if let Some(decode_parms) = decode_parms {
            dict.replace_key_unchecked(b"/DecodeParms", decode_parms);
        }
        if length == 0 {
            dict.remove_key(b"/Length");
        } else {
            dict.replace_key_unchecked(
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

    /// Enable or disable qpdf writer filtering for this stream.
    ///
    /// This is qpdf's `QPDFObjectHandle::setFilterOnWrite`
    /// (`include/qpdf/QPDFObjectHandle.hh:972-982`,
    /// `libqpdf/QPDFObjectHandle.cc:1265-1268`). When disabled, the writer
    /// must not decode, normalize, or recompress the stream, even when the
    /// stream is modified or the writer requests a non-none decode level.
    /// The setting belongs to the canonical stream value, so cloned handles
    /// observe the same state. It is not serialized and does not mark the
    /// PDF object dirty; the mutation generation is advanced so a writer
    /// cache made before this call cannot reuse an obsolete filtering result.
    pub fn set_filter_on_write(&self, value: bool) -> Result<()> {
        self.try_dereference()?;
        let is_stream = self.with_value_mut(|state| match state {
            Some(ObjectValue::Stream {
                filter_on_write, ..
            }) => {
                *filter_on_write = value;
                true
            }
            _ => false,
        });
        if is_stream {
            Ok(())
        } else {
            let type_name = self.type_name()?;
            Err(Error::System(format!(
                "operation for stream attempted on object of type {type_name}"
            )))
        }
    }

    /// Return whether qpdf's writer may filter this stream.
    ///
    /// This is qpdf's `QPDFObjectHandle::getFilterOnWrite`
    /// (`include/qpdf/QPDFObjectHandle.hh:972-982`,
    /// `libqpdf/QPDFObjectHandle.cc:1271-1273`).
    pub fn get_filter_on_write(&self) -> Result<bool> {
        self.try_dereference()?;
        let value = self.with_value(|state| match state {
            Some(ObjectValue::Stream {
                filter_on_write, ..
            }) => Some(*filter_on_write),
            _ => None,
        });
        if let Some(value) = value {
            return Ok(value);
        }
        let type_name = self.type_name()?;
        Err(Error::System(format!(
            "operation for stream attempted on object of type {type_name}"
        )))
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

    /// Whether this stream currently uses a deferred provider source rather
    /// than replacement bytes or its parsed original source. This is kept at
    /// the canonical stream boundary so qpdf's foreign-copy path can preserve
    /// the different source-lifetime contracts of provider and file-backed
    /// streams.
    pub(crate) fn has_stream_data_provider(&self) -> bool {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream {
                stream_provider, ..
            }) => stream_provider.is_some(),
            _ => false,
        })
    }

    /// The parsed source length retained by an original stream, when this
    /// handle currently contains a stream. qpdf stores this alongside the
    /// source offset in `QPDF_Stream`; the foreign-copy provider captures it
    /// without retaining the source `ObjectHandle` itself.
    pub(crate) fn stream_source_length(&self) -> Option<usize> {
        self.with_value(|value| match value {
            Some(ObjectValue::Stream { stream_length, .. }) => Some(*stream_length),
            _ => None,
        })
    }

    /// Whether qpdf must treat this stream's data as modified even when its
    /// original encoded payload is still available. `QPDF_Stream::isDataModified`
    /// is true as soon as a token filter is registered
    /// (`libqpdf/QPDF_Stream.cc:321-324`); the writer uses that bit to avoid
    /// copying a lone `/FlateDecode` source without running the filter
    /// (`libqpdf/QPDFWriter.cc:1234-1315`).
    pub(crate) fn is_data_modified(&self) -> bool {
        if !self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. }))) {
            return false;
        }
        let filters = self.0.borrow().stream_token_filters.clone();
        let modified = !filters.borrow().is_empty();
        modified
    }

    /// Whether the current stream bytes have already passed the explicit
    /// content-normalization consumer. This is intentionally not inferred
    /// from `replaceStreamData`: qpdf's replacement API is also used for
    /// arbitrary caller data that still needs QDF normalization.
    pub(crate) fn content_normalization_applied(&self) -> bool {
        self.0.borrow().content_normalization_applied.get()
    }

    /// Mark the current stream bytes as normalized by the content consumer.
    /// The marker follows the shared payload state, just like qpdf's stream
    /// token-filter list, so aliases do not cause a second normalization pass.
    #[doc(hidden)]
    pub fn mark_content_normalization_applied(&self) {
        self.set_content_normalization_applied(true);
    }

    fn set_content_normalization_applied(&self, applied: bool) {
        let marker = self.0.borrow().content_normalization_applied.clone();
        marker.set(applied);
        for owner in self.state_owner_handles() {
            owner.0.borrow_mut().content_normalization_applied = marker.clone();
        }
        self.bump_mutation_generation();
    }

    /// Register a qpdf-style lazy token filter on this stream. The original
    /// source bytes remain untouched; the filter is inserted into the decoded
    /// stream pipeline only when a filtering pipe is requested.
    pub fn add_token_filter(&self, filter: Rc<RefCell<dyn TokenFilter>>) -> Result<()> {
        self.try_dereference()?;
        if !self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. }))) {
            let type_name = self.type_name()?;
            return Err(Error::System(format!(
                "operation for stream attempted on object of type {}",
                type_name
            )));
        }
        let filters = self.0.borrow().stream_token_filters.clone();
        // A canonical object can have distinct handle slots that share one
        // payload state (for example a dictionary child and the document's
        // object-cache handle). qpdf's token-filter list belongs to that one
        // stream allocation, so make every state owner observe the same list
        // before registering the callback.
        for owner in self.state_owner_handles() {
            owner.0.borrow_mut().stream_token_filters = filters.clone();
        }
        filters.borrow_mut().push(filter);
        self.bump_mutation_generation();
        Ok(())
    }

    /// Coalesce a page's content array through the canonical lazy provider and
    /// attach a token filter to the replacement stream, matching qpdf's
    /// `QPDFObjectHandle::addContentTokenFilter` (`:1850-1854`).
    pub fn add_content_token_filter(&self, filter: Rc<RefCell<dyn TokenFilter>>) -> Result<()> {
        self.coalesce_content_streams()?;
        self.try_get_key(b"/Contents")?.add_token_filter(filter)
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
    /// `encode_flags` is a bitwise OR of [`STREAM_ENCODE_COMPRESS`] and
    /// [`STREAM_ENCODE_NORMALIZE`], qpdf's `qpdf_ef_compress` and
    /// `qpdf_ef_normalize` bits. The output stages are built first, then
    /// the stream filters are added in reverse `/Filter` order. The source
    /// is finally dispatched through the completed chain.
    #[allow(clippy::too_many_arguments)]
    pub fn pipe_stream_data(
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
        if !filtering_attempted {
            let filename = self
                .context()
                .map(|context| context.input_description())
                .unwrap_or_default();
            return Err(Error::Unsupported(format_qpdf_exception_what(
                &filename,
                "",
                self.get_parsed_offset(),
                "getStreamData called on unfilterable stream",
            )));
        }
        if !stream_data_succeeded {
            return Err(Error::Unsupported(
                "error getting decoded stream data".to_owned(),
            ));
        }
        Ok(Rc::new(buffer.take_buffer()?))
    }

    /// Write qpdf's extended stream JSON representation.
    ///
    /// This is `QPDF_Stream::writeStreamJSON`
    /// (`libqpdf/QPDF_Stream.cc:207-295`), kept on the canonical stream handle
    /// rather than split between a separate stream payload reader and
    /// a document-level dictionary writer. `json_data` matches qpdf's three
    /// `qpdf_json_stream_data_e` values. The optional pipeline is the side-file
    /// sink required by `File`; it is deliberately not the JSON output sink.
    ///
    /// The returned decode level is the effective level used after qpdf's
    /// filtered-to-raw retry. Callers that do not need it may discard it, but
    /// retaining it is what lets a future `getStreamJSON` facade attach an
    /// inline blob to the exact successful decode level.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_stream_json(
        &self,
        json_version: i32,
        out: &mut dyn Pipeline,
        json_data: QpdfStreamJsonData,
        mut decode_level: DecodeLevel,
        pipeline: Option<&mut dyn Pipeline>,
        data_filename: &str,
        no_data_key: bool,
        depth: usize,
    ) -> std::result::Result<DecodeLevel, ObjectJsonError> {
        if !matches!(json_version, 1 | 2) {
            return Err(ObjectJsonError::UnsupportedVersion(json_version));
        }

        match json_data {
            QpdfStreamJsonData::None | QpdfStreamJsonData::Inline if pipeline.is_some() => {
                return Err(ObjectJsonError::Pdf(
                    "QPDF_Stream::writeStreamJSON: pipeline should only be supplied when json_data is file"
                        .to_owned(),
                ));
            }
            QpdfStreamJsonData::File if pipeline.is_none() => {
                return Err(ObjectJsonError::Pdf(
                    "QPDF_Stream::writeStreamJSON: pipeline must be supplied when json_data is file"
                        .to_owned(),
                ));
            }
            QpdfStreamJsonData::File if data_filename.is_empty() => {
                return Err(ObjectJsonError::Pdf(
                    "QPDF_Stream::writeStreamJSON: data_filename must be supplied when json_data is file"
                        .to_owned(),
                ));
            }
            _ => {}
        }

        self.try_dereference()
            .map_err(|error| ObjectJsonError::Pdf(error.to_string()))?;
        let stream_dict = self.as_stream_dict().ok_or_else(|| {
            ObjectJsonError::Pdf(
                "QPDF_Stream::writeStreamJSON called on a non-stream object".to_owned(),
            )
        })?;

        let mut stream_first = true;
        Json::write_dictionary_open(out, &mut stream_first, depth)?;

        if matches!(json_data, QpdfStreamJsonData::None) {
            Json::write_dictionary_key(out, &mut stream_first, b"dict", depth + 1)?;
            stream_dict.write_json(json_version, out, false, depth + 1)?;
            Json::write_dictionary_close(out, stream_first, depth)?;
            return Ok(decode_level);
        }

        let mut payload = None;
        let mut filtered = false;
        let mut filter = !matches!(decode_level, DecodeLevel::None);
        for attempt in 1..=2 {
            let mut buffer = Buffer::new("stream data", None);
            let mut discard = Discard;
            let data_pipeline: &mut dyn Pipeline =
                if no_data_key && matches!(json_data, QpdfStreamJsonData::Inline) {
                    &mut discard
                } else {
                    &mut buffer
                };
            let mut filtering_attempted = false;
            let succeeded = self
                .pipe_stream_data(
                    data_pipeline,
                    &mut filtering_attempted,
                    0,
                    decode_level,
                    false,
                    attempt == 1,
                )
                .map_err(|error| ObjectJsonError::Pdf(error.to_string()))?;
            if !succeeded || (filter && !filtering_attempted) {
                filter = false;
                decode_level = DecodeLevel::None;
                continue;
            }

            filtered = filter && filtering_attempted;
            if !no_data_key || !matches!(json_data, QpdfStreamJsonData::Inline) {
                buffer.finish().map_err(ObjectJsonError::Pipeline)?;
                payload = Some(buffer.take_buffer().map_err(ObjectJsonError::Pipeline)?);
            } else {
                payload = Some(Vec::new());
            }
            break;
        }

        let payload = payload.ok_or_else(|| {
            ObjectJsonError::Pdf("QPDF_Stream: failed to get stream data".to_owned())
        })?;

        let dict = stream_dict
            .shallow_copy()
            .map_err(|error| ObjectJsonError::Pdf(error.to_string()))?;
        dict.remove_key(b"/Length");
        if filter && filtered {
            dict.remove_key(b"/Filter");
            dict.remove_key(b"/DecodeParms");
        }

        match json_data {
            QpdfStreamJsonData::Inline if !no_data_key => {
                Json::write_dictionary_item(
                    out,
                    &mut stream_first,
                    b"data",
                    &Json::make_blob(move |sink| sink.write(&payload)),
                    depth + 1,
                )?;
            }
            QpdfStreamJsonData::Inline => {}
            QpdfStreamJsonData::File => {
                Json::write_dictionary_item(
                    out,
                    &mut stream_first,
                    b"datafile",
                    &Json::make_string(data_filename.as_bytes()),
                    depth + 1,
                )?;
                pipeline
                    .expect("file mode pipeline was validated above")
                    .write(&payload)?;
            }
            QpdfStreamJsonData::None => {
                unreachable!("none mode returned before payload piping") // cov:ignore: none mode returns before payload piping
            }
        }

        Json::write_dictionary_key(out, &mut stream_first, b"dict", depth + 1)?;
        dict.write_json(json_version, out, false, depth + 1)?;
        Json::write_dictionary_close(out, stream_first, depth)?;
        Ok(decode_level)
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
        let token_filters = {
            let filters = self.0.borrow().stream_token_filters.clone();
            let token_filters = filters.borrow().clone();
            token_filters
        };
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
        // `QPDF_Stream::pipeStreamData` constructs the filtering stages only
        // when an encode/decode policy is requested (`QPDF_Stream.cc:488-520`).
        // `isDataModified` is consumed by `QPDFWriter::willFilterStream` to
        // decide whether the writer must enter this pipe at all; it does not
        // by itself turn Preserve-mode pipe calls into decoded token-filter
        // calls.
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

        // qpdf's source order is decode filters -> token filters -> content
        // normalization -> encoding. Since `head` is assembled from the
        // sink backwards, token filters are wrapped before the decode stages
        // in reverse registration order (`QPDF_Stream.cc:488-620`).
        for filter in token_filters.into_iter().rev() {
            let tokenizer = QpdfTokenizer::new_shared("stream token filter", filter, Some(head));
            head = PipelineRef::Owned(Box::new(tokenizer));
        }

        for filter in plan.filters.iter_mut().rev() {
            let warning_handle = self.clone();
            let warning_delivery_error = Rc::clone(&warning_delivery_error);
            let suppress_filter_warnings = suppress_warnings;
            filter.set_warning_callback(Box::new(move |message, _code| {
                if suppress_filter_warnings {
                    return Ok(());
                }
                match warning_handle.stream_data_warning(message) {
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
                    self.stream_data_warning(&warning)?;
                }
            }
        }
        Ok(success)
    }

    /// Report a warning raised by `QPDF_Stream::pipeStreamData` with the
    /// stream-data location, not the generic object-warning description.
    ///
    /// qpdf's `QPDF_Stream::warn` (`libqpdf/QPDF_Stream.cc:695-698`) routes
    /// these messages through `QPDF::warn(..., parsed_offset, ...)`, so a
    /// parsed stream warning is rendered as `file (offset N): message` and
    /// retains that offset in the document warning collection. Programmatic
    /// streams have no parsed offset; they retain the ordinary
    /// `QPDFObjectHandle::objectWarning` fallback.
    fn stream_data_warning(&self, message: &str) -> Result<()> {
        let offset = self.get_parsed_offset();
        if offset >= 0 {
            if let Some(context) = self.context() {
                return context.warn_stream_data(offset as u64, None, message.to_owned());
            } // cov:ignore: the return above makes this LLVM closing-branch artifact unreachable
        }
        self.object_warning(message)
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
                stream_dict.replace_key_unchecked(b"/Length", ObjectHandle::integer(actual_length));
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

    /// Return the qpdf-compatible numeric type code after resolving this
    /// handle's own indirect object: `include/qpdf/Constants.h:108-127`'s
    /// `qpdf_object_type_e` ordinals. qpdf's own `getTypeCode()`/`getTypeName()`
    /// (`include/qpdf/QPDFObjectHandle.hh:311-316`,
    /// `libqpdf/QPDFObjectHandle.cc:240-250`) call `dereference()` before
    /// inspecting the value. The internal `try_dereference` helper mirrors that
    /// handle-layer responsibility and propagates resolver failures through
    /// [`crate::Result`].
    ///
    /// Reserved and destroyed handles are checked before resolution and retain
    /// qpdf's `1` (`ot_reserved`) and `14` (`ot_destroyed`) ordinals. The
    /// An uninitialized handle returns qpdf's `ot_uninitialized` code (`0`)
    /// without entering the resolver, matching `getTypeCode`'s null-handle
    /// fallback (`libqpdf/QPDFObjectHandle.cc:240-244`).
    ///
    /// # Errors
    ///
    /// Returns the resolver error when an unresolved indirect handle cannot be
    /// resolved, such as when its owning document has been dropped.
    pub fn type_code(&self) -> Result<u8> {
        if !self.is_initialized() {
            return Ok(0);
        }
        self.try_dereference()?;
        self.with_value(|value| {
            Ok(value
                .expect("every reachable state here (direct or indirect Resolved) carries a value")
                .type_code())
        })
    }

    /// The qpdf-compatible type name string for this handle's value.
    /// `QPDFObjectHandle::getTypeName` dereferences and delegates to
    /// `QPDFObject::getTypeName` (`libqpdf/QPDFObjectHandle.cc:247-250`),
    /// which reads the value-layer `type_name` field. An uninitialized handle
    /// returns `"uninitialized"`; other resolution errors are propagated
    /// unchanged.
    pub fn type_name(&self) -> Result<&'static str> {
        if !self.is_initialized() {
            return Ok("uninitialized");
        }
        self.try_dereference()?;
        self.with_value(|value| {
            Ok(value
                .expect("every reachable state here (direct or indirect Resolved) carries a value")
                .type_name())
        })
    }

    /// Write this handle through qpdf 11.9.0's `QPDFObjectHandle::writeJSON`
    /// pipeline boundary.
    ///
    /// The writer is deliberately owned by the handle layer: qpdf dispatches
    /// from `QPDFObjectHandle::writeJSON` into each `QPDF_*::writeJSON`
    /// implementation using one `JSON::Writer`, and the caller retains the
    /// outer pipeline's `finish` boundary. `dereference_indirect` applies only
    /// to this handle. Array and dictionary children use qpdf's ordinary
    /// non-dereferencing child dispatch, so an indirect child remains an
    /// `"N G R"` string even when the parent was requested with
    /// `dereference_indirect = true`.
    ///
    /// Correspondence: `libqpdf/QPDFObjectHandle.cc:1630-1647`,
    /// `QPDF_Array.cc:153-187`, `QPDF_Dictionary.cc:72-95`, and
    /// `qpdf/JSON_writer.hh:16-135`.
    pub(crate) fn write_json(
        &self,
        json_version: i32,
        out: &mut dyn Pipeline,
        dereference_indirect: bool,
        depth: usize,
    ) -> std::result::Result<(), ObjectJsonError> {
        if !matches!(json_version, 1 | 2) {
            return Err(ObjectJsonError::UnsupportedVersion(json_version));
        }
        ObjectJsonWriter::new(out, depth).write_handle(
            self,
            json_version,
            dereference_indirect,
            depth,
        )
    }

    /// The qpdf `QPDFObjectHandle::getJSON` wrapper around [`Self::write_json`].
    ///
    /// `PlString` is the flpdf equivalent of qpdf's `Pl_Buffer` at this
    /// boundary. It is intentionally not exposed as a new serializer path:
    /// `get_json` writes through the same canonical handle writer and only
    /// parses the completed bytes after that writer returns.
    pub(crate) fn get_json(
        &self,
        json_version: i32,
        dereference_indirect: bool,
    ) -> std::result::Result<crate::json::Json, ObjectJsonError> {
        let mut bytes = Vec::new();
        {
            let mut out = PlString::new("object json", None, &mut bytes);
            self.write_json(json_version, &mut out, dereference_indirect, 0)?;
        }
        crate::json::Json::parse(&bytes).map_err(|error| ObjectJsonError::Json(error.to_string()))
    }

    // `try_dereference` owns the explicit resolution boundary. Once the
    // handle is resolved, including to qpdf's internal Unresolved/Reserved/
    // Destroyed value, this helper exposes the actual ObjectValue.
    pub(crate) fn with_value<T>(&self, f: impl FnOnce(Option<&ObjectValue>) -> T) -> T {
        let state = self.0.borrow().state.clone();
        let state = state.borrow();
        f(Some(&state))
    }

    // Mutable twin of `with_value`: every resolved qpdf value, including the
    // internal sentinels, is a real mutable value-layer slot.
    fn with_value_mut<T>(&self, f: impl FnOnce(Option<&mut ObjectValue>) -> T) -> T {
        let state = self.0.borrow().state.clone();
        let result = {
            let mut state = state.borrow_mut();
            f(Some(&mut state))
        };
        self.bump_mutation_generation();
        result
    }

    fn bump_mutation_generation(&self) {
        let generation = self.0.borrow().mutation_generation.clone();
        generation.set(generation.get().wrapping_add(1));
    }

    pub(crate) fn mutation_fingerprint(&self) -> (usize, u64) {
        let generation = self.0.borrow().mutation_generation.clone();
        (Rc::as_ptr(&generation) as usize, generation.get())
    }

    /// This handle's qpdf-syntax unparse form
    /// (`include/qpdf/QPDFObjectHandle.hh:1159`,
    /// `libqpdf/QPDFObjectHandle.cc:1574-1584`): an indirect handle always
    /// unparses to its own `"N G R"`, regardless of resolution state; a
    /// direct handle delegates to [`Self::unparse_resolved`].
    pub fn unparse(&self) -> Vec<u8> {
        match self.object_ref() {
            Some(object_ref) => object_ref.to_string().into_bytes(),
            None => self.unparse_resolved(),
        }
    }

    /// This handle's resolved qpdf-syntax form
    /// (`libqpdf/QPDFObjectHandle.cc:1586-1593`). The direct serializer walks
    /// `ObjectValue` and child [`ObjectHandle`] values without constructing
    /// a detached value tree. It resolves the receiver before
    /// dispatch, resolves array/dictionary children in qpdf's order, omits
    /// dictionary entries whose values resolve to null, and keeps indirect
    /// children in reference form (`libqpdf/QPDF_Array.cc:122-149`,
    /// `libqpdf/QPDF_Dictionary.cc:58-68`). An indirect stream remains its
    /// own reference because `QPDF_Stream::unparse` returns that form
    /// (`libqpdf/QPDF_Stream.cc:173-178`).
    ///
    /// This non-fallible convenience method retains the existing null fallback
    /// when the qpdf operation would throw. Call [`Self::try_unparse_resolved`]
    /// at production error boundaries that need the qpdf error classification.
    pub fn unparse_resolved(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if unparse_resolved_into(self, &mut out, false).is_err() {
            out.clear();
            out.extend_from_slice(b"null");
        }
        out
    }

    /// This handle's resolved qpdf-syntax form through a fallible error
    /// boundary, porting `QPDFObjectHandle::unparseResolved`
    /// (`libqpdf/QPDFObjectHandle.cc:1586-1593`). Unlike
    /// [`Self::unparse_resolved`], this variant first resolves an unresolved
    /// indirect handle and preserves qpdf's logic errors for reserved and
    /// destroyed values (`QPDF_Reserved::unparse`,
    /// `libqpdf/QPDF_Reserved.cc:22-26`; `QPDF_Destroyed::unparse`,
    /// `libqpdf/QPDF_Destroyed.cc:24-29`).
    ///
    /// The fallible qpdf-shaped error boundary for resolved serialization.
    ///
    /// `QPDFObjectHandle::unparseResolved` first dereferences the receiver and
    /// then delegates to `QPDFObject::unparse`; the value implementations throw
    /// for unresolved, reserved, and destroyed values
    /// (`libqpdf/QPDFObjectHandle.cc:1586-1593`,
    /// `libqpdf/QPDF_Unresolved.cc:23-27`,
    /// `libqpdf/QPDF_Reserved.cc:22-26`,
    /// `libqpdf/QPDF_Destroyed.cc:24-28`). This method preserves those errors
    /// while the non-fallible [`Self::unparse_resolved`] facade maps them to its
    /// established `null` fallback.
    pub fn try_unparse_resolved(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        unparse_resolved_into(self, &mut out, true)?;
        Ok(out)
    }

    /// Replace this handle's own value in place, preserving its identity
    /// (every other outstanding clone observes the new value) and its
    /// already-recorded parsed offset (`parsed_offset` is untouched here --
    /// see [`Self::reset_parsed_offset`] to clear it). A no-op for an
    /// indirect handle; see [`Self::set_resolved`] for that case.
    ///
    /// Used by canonical replacement code to update a stream's own dictionary handle
    /// in place when the replacement value is also a stream, so the
    /// dictionary handle's already-recorded `<<`-start parsed offset
    /// survives instead of being lost to a freshly minted handle.
    #[cfg(test)]
    pub(crate) fn replace_direct_value(&self, value: ObjectValue) {
        if self.is_direct() {
            self.0.borrow().stream_token_filters.borrow_mut().clear();
            self.replace_shared_state(canonicalize_object_value(value));
        }
    }

    /// Reset this handle's parsed offset back to the no-offset sentinel,
    /// overriding the set-once contract [`Self::set_parsed_offset_if_unset`]
    /// normally enforces.
    ///
    /// Used by canonical replacement code: once it replaces an indirect handle's
    /// value with a caller-supplied one, any previously recorded source
    /// position no longer describes that value.
    pub(crate) fn reset_parsed_offset(&self) {
        self.0.borrow_mut().parsed_offset = NO_PARSED_OFFSET;
    }
}

// Direct `QPDFObjectHandle::unparseResolved` serialization. The value and
// child handles remain in the canonical ObjectHandle graph; no detached raw
// `Object` projection is created. Dictionary null suppression follows
// `QPDF_Dictionary::unparse` (`libqpdf/QPDF_Dictionary.cc:58-68`), while array
// elements retain nulls (`libqpdf/QPDF_Array.cc:122-149`).
fn unparse_resolved_into(handle: &ObjectHandle, out: &mut Vec<u8>, strict: bool) -> Result<()> {
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, || {
        handle.try_dereference()?;
        let value = handle
            .with_value(|value| value.cloned())
            .expect("every ObjectHandle carries a value state");

        if handle.object_ref().is_some() && matches!(value, ObjectValue::Stream { .. }) {
            write_unparse_reference(handle.object_ref().expect("checked above"), out);
            return Ok(());
        }

        unparse_resolved_value(handle, value, out, strict)
    })
}

// `QPDF_Array::unparse` resolves each element only to read its own object/
// generation identity (`item.second->resolve(); auto og =
// item.second->getObjGen();`, `libqpdf/QPDF_Array.cc:130-132`), but that
// identity is a property of the *handle itself*, not of whatever it resolves
// to — an indirect handle already carries its object/generation number
// before resolution ever runs. Checking `object_ref()` first, the same way
// `ObjectRef` is already known on this handle, avoids resolving a dangling or
// malformed indirect child purely to report its own indirectness; qpdf's own
// array unparse never needs that resolution to succeed to emit `N G R` for
// such a child.
fn unparse_resolved_child(handle: &ObjectHandle, out: &mut Vec<u8>, strict: bool) -> Result<()> {
    if let Some(object_ref) = handle.object_ref() {
        write_unparse_reference(object_ref, out);
        Ok(())
    } else {
        unparse_resolved_into(handle, out, strict)
    }
}

fn unparse_resolved_value(
    handle: &ObjectHandle,
    value: ObjectValue,
    out: &mut Vec<u8>,
    strict: bool,
) -> Result<()> {
    match value {
        ObjectValue::Null => out.extend_from_slice(b"null"),
        ObjectValue::Unresolved => {
            if strict {
                return Err(unresolved_unparse_error());
            }
            out.extend_from_slice(b"null");
        }
        ObjectValue::Reserved => {
            if strict {
                return Err(reserved_unparse_error());
            }
            out.extend_from_slice(b"null");
        }
        ObjectValue::Destroyed => {
            if strict {
                return Err(destroyed_unparse_error());
            }
            out.extend_from_slice(b"null");
        }
        ObjectValue::Boolean(value) => {
            out.extend_from_slice(if value { b"true" } else { b"false" });
        }
        ObjectValue::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        ObjectValue::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
        ObjectValue::RealLiteral { value, literal } => {
            if crate::pdf_syntax::real_literal_is_safe(&literal, value) {
                out.extend_from_slice(&literal);
            } else {
                out.extend_from_slice(value.to_string().as_bytes());
            }
        }
        ObjectValue::Name(value) => {
            out.push(b'/');
            crate::pdf_syntax::write_name_escaped(out, &value);
        }
        ObjectValue::String(value) => crate::pdf_syntax::write_string_value(out, &value),
        ObjectValue::Operator(value) | ObjectValue::InlineImage(value) => {
            out.extend_from_slice(&value);
        }
        ObjectValue::Array(children) => {
            out.extend_from_slice(b"[ ");
            for child in children {
                unparse_resolved_child(&child, out, strict)?;
                out.push(b' ');
            }
            out.push(b']');
        }
        ObjectValue::Dictionary(entries) => {
            out.extend_from_slice(b"<< ");
            for (key, child) in entries {
                if child.try_is_null()? {
                    continue;
                }
                write_unparse_dictionary_key(out, &key);
                out.push(b' ');
                unparse_resolved_child(&child, out, strict)?;
                out.push(b' ');
            }
            out.extend_from_slice(b">>");
        }
        ObjectValue::Stream {
            stream_dict,
            stream_data,
            ..
        } => {
            // qpdf owns streams as indirect objects. This direct-stream arm
            // retains the existing Rust-only factory behavior for a value
            // without an object number.
            unparse_resolved_into(&stream_dict, out, strict)?;
            out.extend_from_slice(b"\nstream\n");
            let data = match stream_data {
                Some(data) => data,
                None => handle.get_raw_stream_data()?,
            };
            out.extend_from_slice(&data);
            out.extend_from_slice(b"\nendstream");
        }
    }
    Ok(())
}

fn write_unparse_reference(object_ref: ObjectRef, out: &mut Vec<u8>) {
    out.extend_from_slice(object_ref.to_string().as_bytes());
}

fn write_unparse_dictionary_key(out: &mut Vec<u8>, key: &[u8]) {
    if let Some((&first, tail)) = key.split_first() {
        // QPDF_Name::normalizeName preserves the first byte and escapes only
        // the remainder (`libqpdf/QPDF_Name.cc:27-49`).
        out.push(first);
        crate::pdf_syntax::write_name_escaped(out, tail);
    }
}

// Stack growth uses the same red-zone and growth size for direct serialization,
// makeDirect, and the other recursive ObjectHandle walkers in this module.
const UNPARSE_STACK_RED_ZONE: usize = 32 * 1024;
const UNPARSE_STACK_GROWTH_SIZE: usize = 1024 * 1024;

fn reserved_unparse_error() -> Error {
    Error::System("QPDFObjectHandle: attempting to unparse a reserved object".to_owned())
}

fn unresolved_unparse_error() -> Error {
    Error::Internal("attempted to unparse an unresolved QPDFObjectHandle".to_owned())
}

fn destroyed_unparse_error() -> Error {
    Error::Internal("attempted to unparse a QPDFObjectHandle from a destroyed QPDF".to_owned())
}

fn unresolved_copy_error() -> Error {
    Error::Internal("attempted to shallow copy an unresolved QPDFObjectHandle".to_owned())
}

fn destroyed_copy_error() -> Error {
    Error::Internal("attempted to shallow copy QPDFObjectHandle from destroyed QPDF".to_owned())
}

// Unlike `reserved_unparse_error` above, no qpdf throw text exists to mirror
// here: `QPDF::makeIndirectObject` is never called with a reserved handle
// anywhere in qpdf's own source (see `ObjectHandle::direct_value_clone`'s
// own doc for the call-site survey), and `QPDF_Reserved::copy` itself never
// throws either (`libqpdf/QPDF_Reserved.cc:14-19`). `Error::Unsupported`
// matches the sibling "already indirect" rejection
// `Pdf::make_indirect_object_handle` (`reader.rs`) raises for the case this
// one must not be confused with.
fn reserved_clone_error() -> Error {
    Error::Unsupported(
        "cannot clone a reserved ObjectHandle's value for indirect promotion".to_owned(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QpdfStreamJsonData {
    /// qpdf's `qpdf_sj_none`: emit only the stream dictionary.
    None,
    /// qpdf's `qpdf_sj_inline`: emit payload as a base64 JSON string unless
    /// the caller is using the `getStreamJSON` no-data-key path.
    Inline,
    /// qpdf's `qpdf_sj_file`: emit a datafile name and send payload to the
    /// caller-supplied side-file pipeline.
    File,
}

/// Errors raised by the canonical ObjectHandle JSON writer.
///
/// Pipeline failures stay typed until the caller chooses its public error
/// boundary. qpdf's object-level logic errors remain distinguishable from
/// those failures so get_json and document JSON can preserve their existing
/// conversion/error classifications.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ObjectJsonError {
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
    #[error("non-finite float cannot be serialized as JSON")]
    NonFiniteFloat,
    #[error("PDF error: {0}")]
    Pdf(String),
    #[error("JSON error: {0}")]
    Json(String),
    #[error("QPDF::writeJSON: only version 1 or 2 is supported (got {0})")]
    UnsupportedVersion(i32),
    #[error("QPDFObjectHandle: attempting to get JSON from a reserved object")]
    Reserved,
    #[error("attempted to get JSON from an unresolved QPDFObjectHandle")]
    Unresolved,
    #[error("attempted to dereference an uninitialized QPDFObjectHandle")]
    Uninitialized,
    #[error("attempted to get JSON from a QPDFObjectHandle from a destroyed QPDF")]
    Destroyed,
}

// qpdf's JSON::Writer grows the native stack around recursive object writers
// only indirectly through the caller's traversal. ObjectHandle graphs can be
// built directly in Rust, including a direct dictionary cycle, so retain the
// parser's bounded recursion contract here as well.
const OBJECT_JSON_STACK_RED_ZONE: usize = 32 * 1024;
const OBJECT_JSON_STACK_GROWTH_SIZE: usize = 1024 * 1024;

struct ObjectJsonWriter<'a> {
    out: &'a mut dyn Pipeline,
    first: bool,
    indent: usize,
}

impl<'a> ObjectJsonWriter<'a> {
    const SPACES: &'static [u8; 52] = b",\n                                                  ";
    const SPACE_BLOCK: usize = Self::SPACES.len() - 2;

    fn new(out: &'a mut dyn Pipeline, depth: usize) -> Self {
        Self {
            out,
            first: true,
            indent: 2 * depth,
        }
    }

    fn write_handle(
        &mut self,
        handle: &ObjectHandle,
        json_version: i32,
        dereference_indirect: bool,
        depth: usize,
    ) -> std::result::Result<(), ObjectJsonError> {
        if depth > crate::parser::MAX_PARSE_DEPTH {
            return Err(ObjectJsonError::Pdf(format!(
                "object nesting exceeds maximum depth of {}",
                crate::parser::MAX_PARSE_DEPTH
            )));
        }
        stacker::maybe_grow(
            OBJECT_JSON_STACK_RED_ZONE,
            OBJECT_JSON_STACK_GROWTH_SIZE,
            || self.write_handle_inner(handle, json_version, dereference_indirect, depth),
        )
    }

    fn write_handle_inner(
        &mut self,
        handle: &ObjectHandle,
        json_version: i32,
        dereference_indirect: bool,
        depth: usize,
    ) -> std::result::Result<(), ObjectJsonError> {
        if !handle.is_initialized() {
            return Err(ObjectJsonError::Uninitialized);
        }
        if let Some(object_ref) = handle.object_ref() {
            if !dereference_indirect {
                return self.write_reference(object_ref);
            }
            if handle.is_reserved() {
                return Err(ObjectJsonError::Reserved);
            }
            if !handle.is_resolved() {
                if handle.0.borrow().resolver.is_none() {
                    return Err(ObjectJsonError::Uninitialized);
                }
                handle
                    .try_dereference()
                    .map_err(|error| ObjectJsonError::Pdf(error.to_string()))?;
            }
        }

        if handle.is_reserved() {
            return Err(ObjectJsonError::Reserved);
        }
        match handle
            .type_code()
            .map_err(|error| ObjectJsonError::Pdf(error.to_string()))?
        {
            2 | 11 | 12 => self.write(b"null"),
            3 => self.write(
                if handle
                    .as_boolean()
                    .expect("type_code()? == 3 (boolean) => as_boolean")
                {
                    b"true"
                } else {
                    b"false"
                },
            ),
            4 => {
                let value = handle
                    .as_integer()
                    .expect("type_code()? == 4 (integer) => as_integer");
                self.write(value.to_string().as_bytes())
            }
            5 => self.write_real(handle),
            6 => self.write_string(
                &handle
                    .as_string()
                    .expect("type_code()? == 6 (string) => as_string"),
                json_version,
            ),
            7 => self.write_name_value(
                &handle
                    .as_name()
                    .expect("type_code()? == 7 (name) => as_name"),
                json_version,
            ),
            8 => {
                self.write_start(b'[')?;
                for child in handle
                    .as_array()
                    .expect("type_code()? == 8 (array) => as_array")
                {
                    self.write_next()?;
                    self.write_handle(&child, json_version, false, depth + 1)?;
                }
                self.write_end(b']')
            }
            9 => {
                self.write_start(b'{')?;
                for (key, child) in handle
                    .as_dictionary()
                    .expect("type_code()? == 9 (dictionary) => as_dictionary")
                {
                    // QPDF_Dictionary::writeJSON calls isNull() before
                    // emitting a key. isNull() resolves an indirect child,
                    // so missing/dangling values disappear from the JSON
                    // object while non-null indirect children still emit
                    // their own reference form below.
                    if child.is_reserved() {
                        // A reserved child is not null; its non-dereferenced
                        // identity is still a valid JSON reference.
                    } else if child.object_ref().is_some() && !child.is_resolved() {
                        if child.0.borrow().resolver.is_none() {
                            return Err(ObjectJsonError::Uninitialized);
                        }
                        child
                            .try_dereference()
                            .map_err(|error| ObjectJsonError::Pdf(error.to_string()))?;
                    }
                    if child.is_null() {
                        continue;
                    }
                    self.write_key(&key, json_version)?;
                    self.write_handle(&child, json_version, false, depth + 1)?;
                }
                self.write_end(b'}')
            }
            10 => {
                // QPDF_Stream::writeJSON writes only its dictionary. The
                // outer stream wrapper belongs to QPDF_Stream::writeStreamJSON
                // and the document JSON layer.
                // cov:ignore-start: type_code()? == 10 guarantees that as_stream_dict returns Some
                let dictionary = handle.as_stream_dict().ok_or_else(|| {
                    ObjectJsonError::Pdf("stream's dict handle is not a dictionary".to_string())
                })?;
                // cov:ignore-end
                // QPDF_Stream::writeJSON delegates to its dictionary with the
                // same JSON::Writer, without emitting a stream container of
                // its own (`QPDF_Stream.cc:181-184`). Keep the logical depth
                // unchanged so the bound counts JSON containers only.
                self.write_handle(&dictionary, json_version, false, depth)
            }
            13 => Err(ObjectJsonError::Unresolved),
            14 => Err(ObjectJsonError::Destroyed),
            1 => Err(ObjectJsonError::Reserved), // cov:ignore: reserved values are rejected before type dispatch
            // cov:ignore-start: type_code is exhaustive and every reachable code has a dedicated arm above
            other => Err(ObjectJsonError::Pdf(format!(
                "unsupported qpdf object type code {other}"
            ))),
            // cov:ignore-end
        }
    }

    fn write_real(&mut self, handle: &ObjectHandle) -> std::result::Result<(), ObjectJsonError> {
        let value = handle
            .as_real()
            .expect("type_code()? == 5 (real) => as_real");
        if !value.is_finite() {
            return Err(ObjectJsonError::NonFiniteFloat);
        }
        if let Some((value, literal)) = handle.as_real_literal() {
            let encoded = if crate::pdf_syntax::real_literal_is_safe(&literal, value) {
                literal
            } else {
                value.to_string().into_bytes()
            };
            if encoded.starts_with(b"-.") {
                self.write(b"-0.")?;
                self.write(&encoded[2..])
            } else if encoded.starts_with(b".") {
                self.write(b"0")?;
                self.write(&encoded)
            } else {
                self.write(&encoded)
            }
        } else {
            self.write(value.to_string().as_bytes())
        }
    }

    fn write_string(
        &mut self,
        value: &[u8],
        json_version: i32,
    ) -> std::result::Result<(), ObjectJsonError> {
        if json_version == 1 {
            return self.write_quoted(&utf8_value(value));
        }
        if let Some(rest) = value.strip_prefix(&[0xFE, 0xFF]) {
            return self.write_prefixed_quoted(b"u:", lossy_utf16_to_utf8(rest, false).as_bytes());
        }
        if let Some(rest) = value.strip_prefix(&[0xFF, 0xFE]) {
            return self.write_prefixed_quoted(b"u:", lossy_utf16_to_utf8(rest, true).as_bytes());
        }
        if let Some(rest) = value.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            if std::str::from_utf8(rest).is_ok() {
                return self.write_prefixed_quoted(b"u:", rest);
            }
        }
        if !json_use_hex_string(value) {
            if let Some(text) = decode_pdf_text_string(value) {
                return self.write_prefixed_quoted(b"u:", text.as_bytes());
            }
        }
        self.write(b"\"b:")?;
        let mut hex = Vec::with_capacity(value.len() * 2);
        for &byte in value {
            hex.push(b"0123456789abcdef"[(byte >> 4) as usize]);
            hex.push(b"0123456789abcdef"[(byte & 0x0f) as usize]);
        }
        self.write(&hex)?;
        self.write(b"\"")
    }

    fn write_name_value(
        &mut self,
        value: &[u8],
        json_version: i32,
    ) -> std::result::Result<(), ObjectJsonError> {
        let mut raw = Vec::with_capacity(value.len() + 1);
        raw.push(b'/');
        raw.extend_from_slice(value);
        self.write_name_raw(&raw, json_version)
    }

    fn write_key(
        &mut self,
        key: &[u8],
        json_version: i32,
    ) -> std::result::Result<(), ObjectJsonError> {
        self.write_next()?;
        let raw = if key.first() == Some(&b'/') {
            key.to_vec()
        } else {
            let mut raw = Vec::with_capacity(key.len() + 1);
            raw.push(b'/');
            raw.extend_from_slice(key);
            raw
        };
        // qpdf duplicates QPDF_Name::writeJSON's encoding branches in
        // QPDF_Dictionary::writeJSON so the closing quote and dictionary
        // separator are one Pipeline write (`QPDF_Dictionary.cc:78-90`).
        // Keep that observable chunk boundary here instead of routing through
        // write_name_raw, whose value form must close its quote on its own.
        if json_version == 1 {
            self.write(b"\"")?;
            self.write(&json_encode_string(&json_normalize_name(&raw)))?;
        } else if std::str::from_utf8(&raw).is_ok() {
            self.write(b"\"")?;
            self.write(&json_encode_string(&raw))?;
        } else {
            self.write(b"\"n:")?;
            self.write(&json_encode_string(&json_normalize_name(&raw)))?;
        }
        self.write(b"\": ")
    }

    fn write_name_raw(
        &mut self,
        raw: &[u8],
        json_version: i32,
    ) -> std::result::Result<(), ObjectJsonError> {
        if json_version == 1 {
            return self.write_quoted(&json_normalize_name(raw));
        }
        if std::str::from_utf8(raw).is_ok() {
            self.write_quoted(raw)
        } else {
            self.write(b"\"n:")?;
            self.write(&json_encode_string(&json_normalize_name(raw)))?;
            self.write(b"\"")
        }
    }

    fn write_prefixed_quoted(
        &mut self,
        prefix: &[u8],
        value: &[u8],
    ) -> std::result::Result<(), ObjectJsonError> {
        let mut head = Vec::with_capacity(prefix.len() + 1);
        head.push(b'"');
        head.extend_from_slice(prefix);
        self.write(&head)?;
        self.write(&json_encode_string(value))?;
        self.write(b"\"")
    }

    fn write_quoted(&mut self, value: &[u8]) -> std::result::Result<(), ObjectJsonError> {
        self.write(b"\"")?;
        self.write(&json_encode_string(value))?;
        self.write(b"\"")
    }

    fn write_reference(
        &mut self,
        object_ref: ObjectRef,
    ) -> std::result::Result<(), ObjectJsonError> {
        self.write(b"\"")?;
        self.write(format!("{} {}", object_ref.number, object_ref.generation).as_bytes())?;
        self.write(b" R\"")
    }

    fn write(&mut self, bytes: &[u8]) -> std::result::Result<(), ObjectJsonError> {
        self.out.write(bytes).map_err(ObjectJsonError::from)
    }

    fn write_next(&mut self) -> std::result::Result<(), ObjectJsonError> {
        let spaces = self.indent;
        let remainder = spaces % Self::SPACE_BLOCK;
        if self.first {
            self.first = false;
            self.write(&Self::SPACES[1..remainder + 2])?;
        } else {
            self.write(&Self::SPACES[..remainder + 2])?;
        }
        let mut remaining = spaces;
        while remaining >= Self::SPACE_BLOCK {
            self.write(&Self::SPACES[2..])?;
            remaining -= Self::SPACE_BLOCK;
        }
        Ok(())
    }

    fn write_start(&mut self, delimiter: u8) -> std::result::Result<(), ObjectJsonError> {
        self.write(&[delimiter])?;
        self.first = true;
        self.indent += 2;
        Ok(())
    }

    fn write_end(&mut self, delimiter: u8) -> std::result::Result<(), ObjectJsonError> {
        if self.indent > 1 {
            self.indent -= 2;
        }
        if !self.first {
            self.first = true;
            self.write_next()?;
        }
        self.first = false;
        self.write(&[delimiter])
    }
}

impl Drop for ObjectHandle {
    fn drop(&mut self) {
        Self::drain_owned_descendants(&self.0);
    }
}

impl Drop for ObjectHandleIdentity {
    fn drop(&mut self) {
        ObjectHandle::drain_owned_descendants(&self.0);
    }
}

fn json_use_hex_string(bytes: &[u8]) -> bool {
    let mut non_ascii = 0usize;
    for &byte in bytes {
        match byte {
            0x20..=0x7e => {}
            0x18..=0x1f | 0x7f | 0x80..=0xff => non_ascii += 1,
            0x08 | 0x09 | 0x0a | 0x0c | 0x0d => {}
            _ => return true,
        }
    }
    5 * non_ascii > bytes.len()
}

fn json_normalize_name(raw: &[u8]) -> Vec<u8> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut normalized = Vec::with_capacity(raw.len() + 2);
    normalized.push(raw[0]);
    for &byte in &raw[1..] {
        if byte == 0 {
            normalized.push(b'#');
        } else if !(33..=126).contains(&byte)
            || matches!(
                byte,
                b'#' | b'/' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' | b'[' | b']' | b'%'
            )
        {
            normalized.push(b'#');
            normalized.push(b"0123456789abcdef"[(byte >> 4) as usize]);
            normalized.push(b"0123456789abcdef"[(byte & 0x0f) as usize]);
        } else {
            normalized.push(byte);
        }
    }
    normalized
}

fn json_encode_string(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(value.len());
    for &byte in value {
        match byte {
            b'\\' => encoded.extend_from_slice(b"\\\\"),
            b'"' => encoded.extend_from_slice(b"\\\""),
            b'\x08' => encoded.extend_from_slice(b"\\b"),
            b'\x0c' => encoded.extend_from_slice(b"\\f"),
            b'\n' => encoded.extend_from_slice(b"\\n"),
            b'\r' => encoded.extend_from_slice(b"\\r"),
            b'\t' => encoded.extend_from_slice(b"\\t"),
            0x00..=0x1f => {
                encoded.extend_from_slice(if byte < 0x10 { b"\\u000" } else { b"\\u001" });
                encoded.push(b"0123456789abcdef"[(byte & 0x0f) as usize]);
            }
            _ => encoded.push(byte),
        }
    }
    encoded
}

#[cfg(test)]
mod object_json_writer_tests {
    use super::*;
    use crate::pipeline::PipelineResult;
    use crate::pipeline::PlString;
    use serde_json::Value as JsonValue;
    use std::rc::Rc;

    struct FailOnChunk {
        fail_on: &'static [u8],
        bytes: Vec<u8>,
    }

    impl Pipeline for FailOnChunk {
        fn identifier(&self) -> &str {
            "fail-on-chunk"
        }

        fn write(&mut self, bytes: &[u8]) -> PipelineResult<()> {
            if bytes == self.fail_on {
                return Err(PipelineError::runtime("json sink failed"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }

        // cov:ignore-start: this caller-owned sink is never finished by the JSON writer
        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    struct AlwaysFailStreamResolver;

    impl DocumentResolver for AlwaysFailStreamResolver {
        fn resolve_indirect(&self, _object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
            handle.set_resolved(ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(Vec::new()),
                stream_data: None,
                stream_provider: None,
                filter_on_write: true,
                stream_length: 0,
            });
            Ok(())
        }

        fn pipe_stream_data(
            &self,
            _object_ref: ObjectRef,
            _offset: i64,
            _length: usize,
            _stream_dict: &ObjectHandle,
            _pipeline: &mut dyn Pipeline,
            _suppress_warnings: bool,
            _will_retry: bool,
        ) -> Result<bool> {
            Ok(false)
        }
    }

    #[test]
    fn writer_rejects_a_direct_reserved_handle_before_type_dispatch() {
        let handle = ObjectHandle::new_reserved_direct();
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = handle
            .write_json(2, &mut output, false, 0)
            .expect_err("reserved values have no JSON representation");

        assert!(matches!(error, ObjectJsonError::Reserved));
        assert!(bytes.is_empty());
    }

    #[test]
    fn writer_treats_a_reserved_dictionary_child_as_non_null_then_rejects_it() {
        let handle = ObjectHandle::dictionary(vec![(
            b"Reserved".to_vec(),
            ObjectHandle::new_reserved_direct(),
        )]);
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);

        let error = handle
            .write_json(2, &mut output, false, 0)
            .expect_err("reserved children retain their identity but cannot be written");

        assert!(matches!(error, ObjectJsonError::Reserved));
        assert_eq!(bytes, b"{\n  \"/Reserved\": ");
    }

    #[test]
    fn writer_key_adds_qpdf_name_slash_for_a_raw_consumer_key() {
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);
        let mut writer = ObjectJsonWriter::new(&mut output, 0);

        writer
            .write_key(b"Plain", 2)
            .expect("a raw key is normalized before JSON emission");

        assert_eq!(bytes, b"\n\"/Plain\": ");
    }

    #[test]
    fn writer_key_uses_qpdf_name_normalization_for_json_v1() {
        let mut bytes = Vec::new();
        let mut output = PlString::new("object-handle-json", None, &mut bytes);
        let mut writer = ObjectJsonWriter::new(&mut output, 0);

        writer
            .write_key(b"Plain", 1)
            .expect("JSON v1 keys use qpdf name normalization");

        assert_eq!(bytes, b"\n\"/Plain\": ");
    }

    #[test]
    fn json_name_and_string_helpers_cover_empty_and_json_escape_forms() {
        assert_eq!(json_normalize_name(&[]), Vec::<u8>::new());

        let encoded = json_encode_string(b"\\\"\x08\x0c\n\r\t\x01");
        let expected = vec![
            b'\\', b'\\', b'\\', b'"', b'\\', b'b', b'\\', b'f', b'\\', b'n', b'\\', b'r', b'\\',
            b't', b'\\', b'u', b'0', b'0', b'0', b'1',
        ];
        assert_eq!(encoded, expected);
    }

    fn flate_stream() -> ObjectHandle {
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), ObjectHandle::integer(13)),
                (
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
                (
                    b"/DecodeParms".to_vec(),
                    ObjectHandle::dictionary(Vec::new()),
                ),
            ]),
            Rc::new(vec![
                0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15,
            ]),
        )
    }

    fn write_stream(
        stream: &ObjectHandle,
        mode: QpdfStreamJsonData,
        decode_level: DecodeLevel,
        no_data_key: bool,
        pipeline: Option<&mut dyn Pipeline>,
        data_filename: &str,
    ) -> (Vec<u8>, DecodeLevel) {
        let mut bytes = Vec::new();
        let level = {
            let mut output = PlString::new("stream-json", None, &mut bytes);
            stream
                .write_stream_json(
                    2,
                    &mut output,
                    mode,
                    decode_level,
                    pipeline,
                    data_filename,
                    no_data_key,
                    0,
                )
                .expect("stream JSON should be written")
        };
        (bytes, level)
    }

    #[test]
    fn stream_json_none_preserves_the_original_dictionary_and_omits_data() {
        let stream = flate_stream();
        let (bytes, level) = write_stream(
            &stream,
            QpdfStreamJsonData::None,
            DecodeLevel::Generalized,
            false,
            None,
            "",
        );

        assert_eq!(level, DecodeLevel::Generalized);
        let json: JsonValue = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["dict"]["/Length"], 13);
        assert_eq!(json["dict"]["/Filter"], "/FlateDecode");
        assert!(json.get("data").is_none());
    }

    #[test]
    fn stream_json_inline_decodes_and_normalizes_the_dictionary() {
        let stream = flate_stream();
        let (bytes, level) = write_stream(
            &stream,
            QpdfStreamJsonData::Inline,
            DecodeLevel::Generalized,
            false,
            None,
            "",
        );

        assert_eq!(level, DecodeLevel::Generalized);
        let json: JsonValue = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["data"], "aGVsbG8=");
        assert_eq!(json["dict"], serde_json::json!({}));
    }

    #[test]
    fn stream_json_inline_no_data_key_discards_payload_but_keeps_effective_level() {
        let stream = flate_stream();
        let (bytes, level) = write_stream(
            &stream,
            QpdfStreamJsonData::Inline,
            DecodeLevel::Generalized,
            true,
            None,
            "",
        );

        assert_eq!(level, DecodeLevel::Generalized);
        let json: JsonValue = serde_json::from_slice(&bytes).expect("valid JSON");
        assert!(json.get("data").is_none());
        assert_eq!(json["dict"], serde_json::json!({}));
    }

    #[test]
    fn stream_json_file_writes_the_payload_to_the_supplied_pipeline() {
        let stream = flate_stream();
        let mut side_bytes = Vec::new();
        let (bytes, level) = {
            let mut side = PlString::new("side-file", None, &mut side_bytes);
            write_stream(
                &stream,
                QpdfStreamJsonData::File,
                DecodeLevel::Generalized,
                false,
                Some(&mut side),
                "side-file-7",
            )
        };

        assert_eq!(level, DecodeLevel::Generalized);
        assert_eq!(side_bytes, b"hello");
        let json: JsonValue = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["datafile"], "side-file-7");
        assert_eq!(json["dict"], serde_json::json!({}));
    }

    #[test]
    fn stream_json_retries_an_unfilterable_stream_as_raw_data() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), ObjectHandle::integer(3)),
                (
                    b"/Filter".to_vec(),
                    ObjectHandle::name(b"UnknownDecode".to_vec()),
                ),
            ]),
            Rc::new(b"raw".to_vec()),
        );
        let (bytes, level) = write_stream(
            &stream,
            QpdfStreamJsonData::Inline,
            DecodeLevel::Generalized,
            false,
            None,
            "",
        );

        assert_eq!(level, DecodeLevel::None);
        let json: JsonValue = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(json["data"], "cmF3");
        assert_eq!(json["dict"]["/Filter"], "/UnknownDecode");
        assert!(json["dict"].get("/Length").is_none());
    }

    #[test]
    fn stream_json_validates_pipeline_and_filename_combinations_before_output() {
        let stream = flate_stream();
        let mut side_bytes = Vec::new();
        let mut side = PlString::new("side-file", None, &mut side_bytes);
        let mut output_bytes = Vec::new();
        let mut output = PlString::new("stream-json", None, &mut output_bytes);

        let error = stream
            .write_stream_json(
                3,
                &mut output,
                QpdfStreamJsonData::None,
                DecodeLevel::None,
                None,
                "",
                false,
                0,
            )
            .expect_err("only qpdf JSON versions 1 and 2 are supported");
        assert!(matches!(error, ObjectJsonError::UnsupportedVersion(3)));
        drop(output);
        assert!(output_bytes.is_empty());

        let mut output = PlString::new("stream-json", None, &mut output_bytes);

        let error = stream
            .write_stream_json(
                2,
                &mut output,
                QpdfStreamJsonData::None,
                DecodeLevel::None,
                Some(&mut side),
                "",
                false,
                0,
            )
            .expect_err("none mode rejects a supplied pipeline");
        assert!(
            matches!(error, ObjectJsonError::Pdf(message) if message.contains("pipeline should only"))
        );

        let error = stream
            .write_stream_json(
                2,
                &mut output,
                QpdfStreamJsonData::File,
                DecodeLevel::None,
                None,
                "side-file-7",
                false,
                0,
            )
            .expect_err("file mode requires a pipeline");
        assert!(
            matches!(error, ObjectJsonError::Pdf(message) if message.contains("pipeline must be supplied"))
        );

        let error = stream
            .write_stream_json(
                2,
                &mut output,
                QpdfStreamJsonData::File,
                DecodeLevel::None,
                Some(&mut side),
                "",
                false,
                0,
            )
            .expect_err("file mode requires a filename");
        assert!(
            matches!(error, ObjectJsonError::Pdf(message) if message.contains("data_filename must be supplied"))
        );
        drop(output);
        assert!(output_bytes.is_empty());
    }

    #[test]
    fn stream_json_rejects_a_non_stream_handle() {
        let scalar = ObjectHandle::integer(7);
        let mut bytes = Vec::new();
        let mut output = PlString::new("stream-json", None, &mut bytes);

        let error = scalar
            .write_stream_json(
                2,
                &mut output,
                QpdfStreamJsonData::None,
                DecodeLevel::None,
                None,
                "",
                false,
                0,
            )
            .expect_err("stream JSON requires a stream handle");

        assert!(matches!(
            error,
            ObjectJsonError::Pdf(message)
                if message == "QPDF_Stream::writeStreamJSON called on a non-stream object"
        ));
        assert!(bytes.is_empty());
    }

    #[test]
    fn stream_json_reports_failure_after_both_source_attempts() {
        let resolver = Rc::new(AlwaysFailStreamResolver);
        let resolver_handle: Rc<dyn DocumentResolver> = resolver;
        let stream = ObjectHandle::new_indirect_with_resolver(
            ObjectRef::new(20, 0),
            Rc::downgrade(&resolver_handle),
        );
        let mut bytes = Vec::new();
        let mut output = PlString::new("stream-json", None, &mut bytes);

        let error = stream
            .write_stream_json(
                2,
                &mut output,
                QpdfStreamJsonData::Inline,
                DecodeLevel::Generalized,
                None,
                "",
                false,
                0,
            )
            .expect_err("a source that fails both attempts must be reported");

        assert!(matches!(
            error,
            ObjectJsonError::Pdf(message) if message == "QPDF_Stream: failed to get stream data"
        ));
        assert_eq!(bytes, b"{");
    }

    #[test]
    fn stream_json_propagates_inline_output_failure() {
        let stream = flate_stream();
        let mut output = FailOnChunk {
            fail_on: br#""data": "#,
            bytes: Vec::new(),
        };
        assert_eq!(output.identifier(), "fail-on-chunk");

        let error = stream
            .write_stream_json(
                2,
                &mut output,
                QpdfStreamJsonData::Inline,
                DecodeLevel::Generalized,
                None,
                "",
                false,
                0,
            )
            .expect_err("the output sink failure must be preserved");

        assert!(matches!(
            error,
            ObjectJsonError::Pipeline(PipelineError::Runtime(message))
                if message.as_bytes() == b"json sink failed"
        ));
        assert_eq!(output.bytes, b"{\n  ");
    }

    #[test]
    fn object_json_writer_handles_internal_states_and_real_literals() {
        for (value, expected) in [
            (ObjectValue::Unresolved, ObjectJsonError::Unresolved),
            (ObjectValue::Destroyed, ObjectJsonError::Destroyed),
        ] {
            let handle = ObjectHandle::from_value(value);
            let mut bytes = Vec::new();
            let mut output = PlString::new("object-handle-json", None, &mut bytes);
            let error = handle
                .write_json(2, &mut output, false, 0)
                .expect_err("internal qpdf states are not JSON values");
            assert!(matches!(
                (error, expected),
                (ObjectJsonError::Unresolved, ObjectJsonError::Unresolved)
                    | (ObjectJsonError::Destroyed, ObjectJsonError::Destroyed)
            ));
        }

        for (handle, expected) in [
            (
                ObjectHandle::real_literal(1.2, b"1.2".to_vec()),
                b"1.2".as_slice(),
            ),
            (
                ObjectHandle::real_literal(0.4, b"not-a-real".to_vec()),
                b"0.4".as_slice(),
            ),
        ] {
            let mut bytes = Vec::new();
            let mut output = PlString::new("object-handle-json", None, &mut bytes);
            handle
                .write_json(2, &mut output, false, 0)
                .expect("finite real literals serialize as JSON numbers");
            assert_eq!(bytes, expected);
        }
    }
}

// `ObjectHandle::shallow_copy`'s per-variant dispatch: an Array/Dictionary
// child is recursively shallow-copied through `shallow_copy_child` (which
// re-enters `ObjectHandle::shallow_copy`, the recursion hub carrying its
// own `stacker::maybe_grow` wrap — the same hub-per-call shape as
// `unparse_resolved_into`/`unparse_resolved_child` above), mirroring
// `QPDF_Dictionary::copy`/`QPDF_Array::copy`, which call `shallowCopy` on
// each direct child and keep an indirect one shared. A `Stream` has no
// copy at all: `QPDF_Stream::copy` (`libqpdf/QPDF_Stream.cc:140-145`)
// throws, and it throws from here too — for this value itself and, through
// the recursion, for any direct stream descendant. Every other variant is
// cloned as-is with no further recursion.
fn shallow_copy_value(value: &ObjectValue) -> Result<ObjectValue> {
    Ok(match value {
        ObjectValue::Unresolved => return Err(unresolved_copy_error()),
        ObjectValue::Reserved => ObjectValue::Reserved,
        ObjectValue::Destroyed => return Err(destroyed_copy_error()),
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
            this_val.replace_key(&key, installed)?;
            continue;
        }
        let Some(conflicts_map) = conflicts.as_deref_mut() else {
            continue;
        };
        if og_to_name.is_none() {
            og_to_name = Some(build_og_to_name(this_val));
            rnames = try_get_resource_names(this_val)?;
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
            let new_key = unique_resource_name(&key, &mut min_suffix, &rnames)?;
            conflicts_map
                .entry(rtype.to_vec())
                .or_default()
                .insert(key, new_key.clone());
            this_val.replace_key(&new_key, rval)?;
        }
    }
    Ok(())
}

// `ObjectHandle::merge_resources`'s per-rtype array merge (the
// `this_val.isArray() && other_val.isArray()` arm,
// `libqpdf/QPDFObjectHandle.cc:1130-1146`): union `other_val`'s scalar
// items into `this_val` by unparsed text, appending only what is not
// already present.
fn merge_resource_array(this_val: &ObjectHandle, other_val: &ObjectHandle) -> Result<()> {
    let Some(other_items) = other_val.as_array() else {
        return Ok(()); // cov:ignore: caller already confirmed other_val.as_array().is_some()
    };
    let mut scalars = std::collections::BTreeSet::new();
    for item in this_val.as_array().into_iter().flatten() {
        if is_scalar(&item)? {
            scalars.insert(item.unparse());
        }
    }
    for item in other_items {
        if !is_scalar(&item)? {
            continue;
        }
        let text = item.unparse();
        if scalars.insert(text) {
            append_array_item(this_val, item);
        }
    }
    Ok(())
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

// Mirrors `isScalar()` (`libqpdf/QPDFObjectHandle.cc:449-452`): dereference
// first, then test bool, integer, name, null, real, or string. qpdf performs
// this dereference for every array item rather than relying on a caller-side
// resolution precondition.
pub(crate) fn is_scalar(handle: &ObjectHandle) -> Result<bool> {
    handle.try_dereference()?;
    Ok(handle.as_boolean().is_some()
        || handle.as_integer().is_some()
        || handle.as_name().is_some()
        || handle.is_null()
        || handle.as_real().is_some()
        || handle.as_string().is_some())
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
// looking mismatched against its call site. The receiver and every value are
// dereferenced before their dictionary shape is inspected, as qpdf does.
fn try_get_resource_names(dict: &ObjectHandle) -> Result<std::collections::BTreeSet<Vec<u8>>> {
    dict.try_dereference()?;
    let mut result = std::collections::BTreeSet::new();
    let Some(entries) = dict.as_dictionary() else {
        return Ok(result);
    };
    for (_, value) in entries {
        value.try_dereference()?;
        if value.as_dictionary().is_some() {
            result.extend(value.try_get_keys()?);
        }
    }
    Ok(result)
}

fn object_generation_description(handle: &ObjectHandle) -> String {
    handle
        .object_ref()
        .map(|object_ref| format!("{} {}", object_ref.number, object_ref.generation))
        .unwrap_or_else(|| "0 0".to_owned())
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
) -> Result<Vec<u8>> {
    let mut prefix = key.to_vec();
    prefix.push(b'_');
    let max_suffix = *min_suffix + names.len();
    while *min_suffix <= max_suffix {
        let mut candidate = prefix.clone();
        candidate.extend(min_suffix.to_string().into_bytes());
        if !names.contains(&candidate) {
            return Ok(candidate);
        }
        *min_suffix += 1;
    }
    // Unreachable per qpdf's own invariant: this loop tests strictly more
    // candidates (names.len() + 1) than there are names to conflict with,
    // so by pigeonhole one must be free. qpdf itself treats reaching this
    // point as a coding error and throws std::logic_error
    // (`libqpdf/QPDFObjectHandle.cc:1188-1191`); this maps to `Error::Internal`
    // like every other logic_error in this file.
    // cov:ignore-start: unreachable by the pigeonhole argument above for any input
    Err(Error::Internal(
        "unable to find unconflicting resource name".to_owned(),
    ))
    // cov:ignore-end
}

#[cfg(test)]
mod content_shape_internal_tests {
    use super::*;

    #[test]
    fn supplied_resource_names_do_not_resolve_the_receiver_like_qpdf() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(99, 0), 0);
        let names = std::collections::BTreeSet::from([b"/F0".to_vec()]);
        let mut min_suffix = 0;

        assert_eq!(
            handle
                .get_unique_resource_name(b"/F", &mut min_suffix, Some(&names))
                .unwrap(),
            b"/F1"
        );
    }

    #[test]
    fn all_description_separates_multiple_streams_like_qpdf() {
        let page = ObjectHandle::dictionary(vec![(
            b"/Contents".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
                ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new())),
            ]),
        )]);

        let (streams, all_description) = page.page_contents_with_description().unwrap();

        assert_eq!(streams.len(), 2);
        assert_eq!(all_description, "page object 0 0 stream 0 0, stream 0 0");
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
                filter_on_write: true,
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
        assert!(matches!(
            resolver.original_stream_data_provider(&ObjectHandle::null(), &ObjectHandle::null()),
            Err(Error::Internal(message))
                if message == "original stream data provider requested from a resolver without a document"
        ));
        assert!(!ObjectHandle::integer(1).has_stream_data_provider());
        assert_eq!(ObjectHandle::integer(1).stream_source_length(), None);
        assert!(!resolver.immediate_copy_from());
        assert!(!resolver.pclm_mode());
    }

    #[test]
    fn stream_copy_destination_defaults_delegate_and_warning_defaults_fail() {
        let resolver = NoStreamSourceResolver;
        let destination: Rc<dyn DocumentResolver> = Rc::new(NoStreamSourceResolver);
        let destination_resolver = Rc::downgrade(&destination);

        assert!(matches!(
            resolver.original_stream_data_provider_for_destination(
                &ObjectHandle::null(),
                &ObjectHandle::null(),
                destination_resolver,
            ),
            Err(Error::Internal(message))
                if message == "original stream data provider requested from a resolver without a document"
        ));
        assert!(matches!(
            resolver.warn_stream_data(17, None, "late warning".to_owned()),
            Err(Error::Internal(message))
                if message == "stream data warning requested from a resolver without a document"
        ));
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
            handle.set_resolved(ObjectValue::Null);
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
    fn identity_key_matches_qpdf_object_sameness_without_structural_equality() {
        let original =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(1))]);
        let alias = original.clone();
        let distinct =
            ObjectHandle::dictionary(vec![(b"Value".to_vec(), ObjectHandle::integer(1))]);
        #[allow(
            clippy::mutable_key_type,
            reason = "identity key compares only Rc pointer identity and retains the slot deliberately"
        )]
        let mut seen = std::collections::HashSet::new();

        assert!(seen.insert(original.identity_key()));
        assert!(!seen.insert(alias.identity_key()));
        assert!(seen.insert(distinct.identity_key()));
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
    fn canonical_payload_sharing_rejects_invalid_sources_and_shares_internal_values() {
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
        target
            .share_value_state_with(&destroyed)
            .expect("qpdf QPDFObject::assign accepts a destroyed value payload");
        assert!(target.is_resolved());
        assert_eq!(target.type_code().expect("destroyed type code"), 14);
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
        first.replace_key(b"/Second", second.clone()).unwrap();
        second.replace_key(b"/First", first.clone()).unwrap();
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
        replacement
            .replace_key(b"/Value", ObjectHandle::integer(9))
            .unwrap();
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

        assert_eq!(target.type_code().expect("type code"), 14);
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
        parent.replace_key(b"/Child", child.clone()).unwrap();

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
        first.replace_key(b"/Second", second.clone()).unwrap();
        second.replace_key(b"/First", first.clone()).unwrap();

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
        assert!(matches!(
            scalar.try_has_key(b"/A"),
            Err(Error::System(message))
                if message == "operation for dictionary attempted on object of type integer: returning false for a key containment request"
        ));
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
    fn try_is_stream_of_type_matches_only_a_stream_with_the_requested_dict_type() {
        // Arm 1: a plain (non-stream) dictionary never matches, even when its
        // own /Type says the right thing -- `try_is_stream_of_type` requires
        // `isStream()`, matching `QPDFObjectHandle::isStreamOfType`
        // (`libqpdf/QPDFObjectHandle.cc:468-471`).
        let plain_dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"XRef".to_vec()),
        )]);
        assert!(!plain_dict.try_is_stream_of_type(b"XRef", b"").unwrap());

        // Arm 2: a stream whose nested dictionary has the wrong /Type.
        let wrong_type_dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"ObjStm".to_vec()),
        )]);
        let wrong_type_stream = ObjectHandle::stream(wrong_type_dict, std::rc::Rc::new(Vec::new()));
        assert!(!wrong_type_stream
            .try_is_stream_of_type(b"XRef", b"")
            .unwrap());

        // Arm 3: a stream whose nested dictionary has the requested /Type --
        // the shape every real `/Type /XRef` or `/Type /ObjStm` object has,
        // since both are required to carry stream data.
        let xref_dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"XRef".to_vec()),
        )]);
        let xref_stream = ObjectHandle::stream(xref_dict, std::rc::Rc::new(Vec::new()));
        assert!(xref_stream.try_is_stream_of_type(b"XRef", b"").unwrap());

        let objstm_dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"ObjStm".to_vec()),
        )]);
        let objstm_stream = ObjectHandle::stream(objstm_dict, std::rc::Rc::new(Vec::new()));
        assert!(objstm_stream.try_is_stream_of_type(b"ObjStm", b"").unwrap());
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
            handle
                .replace_key(b"/Resolved", ObjectHandle::boolean(true))
                .unwrap();
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

        original
            .replace_key(b"/Value", ObjectHandle::integer(2))
            .unwrap();
        assert_eq!(promoted.get_key(b"/Value").as_integer(), Some(2));
        promoted
            .replace_key(b"/Value", ObjectHandle::integer(3))
            .unwrap();
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
            filter_on_write: true,
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
        child
            .replace_key(b"/Grandchild", grandchild.clone())
            .unwrap();
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
    fn disconnect_preserves_literal_null_and_resolved_null_as_null() {
        let resolver = resolver();
        let literal_null = ObjectHandle::null();
        literal_null.set_parsed_offset_if_unset(55);
        literal_null.promote_to_indirect(ObjectRef::new(49, 0), 92, Rc::downgrade(&resolver));
        literal_null.disconnect();
        assert!(literal_null.is_direct());
        assert!(literal_null.is_null());
        assert_eq!(literal_null.get_parsed_offset(), 55);

        let resolved_null = ObjectHandle::new_indirect_unresolved(ObjectRef::new(51, 0), -1);
        resolved_null.set_resolved(ObjectValue::Null);
        resolved_null.promote_to_indirect(ObjectRef::new(53, 0), 93, Rc::downgrade(&resolver));
        resolved_null.disconnect();
        assert!(resolved_null.is_direct());
        assert!(resolved_null.is_null());
        assert_eq!(resolved_null.get_parsed_offset(), NO_PARSED_OFFSET);
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
        assert_eq!(alias.type_code().expect("type code"), 14);
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
        assert_eq!(handle.type_code().expect("type code"), 14);
        assert_eq!(handle.get_parsed_offset(), 77);

        handle.disconnect();

        assert!(handle.is_direct());
        assert_eq!(handle.object_ref(), None);
        assert!(!handle.is_null());
        assert_eq!(handle.type_code().expect("type code"), 14);
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
        assert_eq!(stream.type_code().expect("type code"), 10, "ot_stream");
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
    fn real_literal_handle_preserves_the_non_canonical_source_literal() {
        // The preserved-literal value exists so a non-canonical source spelling
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
    use crate::token_filter::TokenFilterOutput;
    use crate::tokenizer::{Token, TokenType};

    struct NoopTokenFilter;

    impl TokenFilter for NoopTokenFilter {
        fn handle_token(
            &mut self,
            _token: &Token,
            _output: &mut TokenFilterOutput<'_>,
        ) -> crate::pipeline::PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn token_filter_registration_rejects_non_stream_handles() {
        let scalar = ObjectHandle::integer(7);
        assert!(!scalar.is_data_modified());

        let mut no_op = NoopTokenFilter;
        let token = Token::new(TokenType::Word, b"q".to_vec());
        let mut output = TokenFilterOutput::new(None);
        no_op.handle_token(&token, &mut output).unwrap();

        let filter: Rc<RefCell<dyn TokenFilter>> = Rc::new(RefCell::new(no_op));
        let error = scalar.add_token_filter(filter).unwrap_err();
        assert!(matches!(
            error,
            Error::System(message)
                if message == "operation for stream attempted on object of type integer"
        ));
        assert!(!scalar.is_data_modified());
    }

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
        copy_dict
            .replace_key(b"/Length", ObjectHandle::integer(7))
            .unwrap();
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

    // `direct_value_clone`'s early `slot.object_ref.is_some()` check already
    // confirms this handle is direct before this match ever runs, so its
    // `Reserved` arm can only be reached by a *direct* reserved handle --
    // only constructible via `ObjectHandle::shallow_copy` on a reserved
    // handle (`QPDF_Reserved::copy`, `libqpdf/QPDF_Reserved.cc:14-19`, never
    // null, never a throw). Codex Review round 5 on PR #789, databaseId
    // 3773627592: this used to fall into the same `Ok(None)` bucket as a
    // genuinely indirect handle, so `Pdf::make_indirect_object_handle`
    // reported "cannot make an already-indirect ObjectHandle indirect" for
    // a handle its own `is_direct()` would confirm is not indirect.
    #[test]
    fn direct_value_clone_rejects_a_direct_reserved_handle_instead_of_reporting_already_indirect() {
        let handle = ObjectHandle::new_reserved_direct();
        assert!(handle.is_direct());

        let error = handle
            .direct_value_clone()
            .expect_err("a direct reserved handle has no ObjectValue to clone");

        assert!(
            !error.to_string().contains("already-indirect"),
            "the handle is direct, not indirect: {error}"
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
            filter_on_write: true,
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
    fn object_slot_shares_object_value_without_a_redundant_state_wrapper() {
        let source = include_str!("object_handle.rs");
        let state_name = ["Object", "State"].concat();
        let resolved_name = [state_name.as_str(), "::Resolved"].concat();
        let state_field = ["state: Rc<RefCell<", "ObjectValue>>"].concat();
        assert!(!source.contains(&format!("enum {state_name}")));
        assert!(!source.contains(&resolved_name));
        assert!(source.contains(&state_field));
    }

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
    fn resolving_to_qpdf_null_resets_the_cached_source_offset() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        handle.set_parsed_offset_if_unset(100);

        handle.set_resolved(ObjectValue::Null);

        assert!(handle.is_null());
        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
    }

    #[test]
    fn set_resolved_null_marks_the_handle_resolved_to_null() {
        // A dangling or broken reference uses the same observable null value
        // as a genuinely parsed literal null.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Null);
        assert!(handle.is_resolved());
        assert!(handle.is_null());
        assert_eq!(handle.as_integer(), None);
    }

    /// A null-resolved indirect handle has the same value-layer state as a
    /// parsed literal null; there is no separate qpdf missing sentinel.
    #[test]
    fn set_resolved_with_a_null_value_uses_the_null_value_layer() {
        let null = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        null.set_resolved(ObjectValue::Null);

        assert!(null.is_resolved());
        assert!(null.is_null());
        let state = null.0.borrow().state.clone();
        assert!(
            matches!(&*state.borrow(), ObjectValue::Null),
            "`set_resolved(Null)` must leave the slot in the value layer"
        );
    }

    #[test]
    fn set_resolved_null_resets_a_previously_recorded_parsed_offset() {
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

        handle.set_resolved(ObjectValue::Null);

        assert_eq!(handle.get_parsed_offset(), NO_PARSED_OFFSET);
        assert!(handle.is_null());
    }

    #[test]
    fn set_resolved_is_a_no_op_on_a_direct_handle() {
        // Direct handles have no resolution state; calling the setter must not
        // panic and must leave the original value untouched.
        let handle = ObjectHandle::integer(42);
        handle.set_resolved(ObjectValue::Integer(99));
        handle.set_resolved(ObjectValue::Null);
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
        assert_eq!(handle.type_code().expect("type code"), 14);
        assert_eq!(handle.as_integer(), None);
    }

    #[test]
    fn disconnect_resets_a_previously_recorded_parsed_offset() {
        // Mirrors `set_resolved_null_resets_a_previously_recorded_parsed_offset`
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
        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert!(format!("{unresolved:?}").contains("Unresolved"));

        let null = ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), 0);
        null.set_resolved(ObjectValue::Null);
        assert!(format!("{null:?}").contains("Resolved(..)"));

        let destroyed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(3, 0), 0);
        destroyed.set_resolved(ObjectValue::Integer(1));
        destroyed.disconnect();
        assert!(format!("{destroyed:?}").contains("Destroyed"));
    }
}

#[cfg(test)]
mod internal_state_value_tests {
    use super::*;
    use crate::writer::object::unparse_object_value;

    #[test]
    fn internal_qpdf_values_use_their_value_layer_error_contracts() {
        for (value, expected) in [
            (
                ObjectValue::Unresolved,
                "attempted to unparse an unresolved QPDFObjectHandle",
            ),
            (
                ObjectValue::Reserved,
                "QPDFObjectHandle: attempting to unparse a reserved object",
            ),
            (
                ObjectValue::Destroyed,
                "attempted to unparse a QPDFObjectHandle from a destroyed QPDF",
            ),
        ] {
            let mut out = Vec::new();
            let unparse_error = unparse_object_value(&value, &mut out)
                .expect_err("internal qpdf values cannot unparse as PDF values");
            assert!(
                unparse_error.to_string().contains(expected),
                "unexpected unparse error: {unparse_error:?}"
            );
        }

        let reserved_copy = shallow_copy_value(&ObjectValue::Reserved).expect("reserved copies");
        assert!(matches!(reserved_copy, ObjectValue::Reserved));

        let unresolved_copy = shallow_copy_value(&ObjectValue::Unresolved)
            .expect_err("unresolved copy is a qpdf logic error");
        assert!(unresolved_copy
            .to_string()
            .contains("attempted to shallow copy an unresolved QPDFObjectHandle"));

        let destroyed_copy = shallow_copy_value(&ObjectValue::Destroyed)
            .expect_err("destroyed copy is a qpdf logic error");
        assert!(destroyed_copy
            .to_string()
            .contains("attempted to shallow copy QPDFObjectHandle from destroyed QPDF"));

        let unresolved = ObjectHandle::from_value(ObjectValue::Unresolved);
        assert_eq!(
            unresolved
                .try_unparse_resolved()
                .expect_err("an unresolved value cannot be unparsed")
                .to_string(),
            "attempted to unparse an unresolved QPDFObjectHandle"
        );
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
        // Mirrors the handle's own `Real(v) | RealLiteral { value: v, .. }`
        // arm — a real-literal value is still "a real"
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
    fn rounded_accessors_return_none_for_an_indirect_handle_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert_eq!(handle.as_boolean(), None);
        assert_eq!(handle.as_real(), None);
        assert!(handle.as_name().is_none());
        assert!(handle.as_string().is_none());
        assert_eq!(handle.object_ref(), Some(ObjectRef::new(9, 0)));
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
    fn object_values_report_qpdf_internal_state_types() {
        let cases = [
            (ObjectValue::Unresolved, 13, "unresolved"),
            (ObjectValue::Reserved, 1, "reserved"),
            (ObjectValue::Destroyed, 14, "destroyed"),
        ];

        for (value, code, name) in cases {
            assert_eq!(value.type_code(), code, "{name}");
            assert_eq!(value.type_name(), name);
        }
    }

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
            assert_eq!(handle.type_code().expect("type code"), *code, "{name}");
            assert_eq!(handle.type_name().expect("type name"), *name);
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
            filter_on_write: true,
            stream_length: 0,
        });
        assert_eq!(stream.type_code().expect("type code"), 10);
        assert_eq!(stream.type_name().expect("type name"), "stream");
    }

    #[test]
    fn type_code_resolves_an_unresolved_indirect_handle_before_classifying() {
        let (handle, _resolver) = crate::object_handle::identity_tests::resolver_bearing_handle(
            ObjectValue::Dictionary(std::collections::BTreeMap::new()),
        );

        assert!(!handle.is_resolved());
        assert_eq!(
            handle.type_code().expect("type code"),
            9,
            "qpdf ot_dictionary"
        );
        assert!(handle.is_resolved());
    }

    #[test]
    fn type_name_resolves_an_unresolved_indirect_handle_before_classifying() {
        let (handle, _resolver) =
            crate::object_handle::identity_tests::resolver_bearing_handle(ObjectValue::Integer(7));

        assert_eq!(handle.type_name().expect("type name"), "integer");
        assert!(handle.is_resolved());
    }

    #[test]
    fn type_code_and_type_name_propagate_resolver_errors() {
        let (handle, _resolver) =
            crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(21, 0));
        assert_eq!(
            handle
                .type_code()
                .expect_err("type classification must be fallible")
                .to_string(),
            "resolver failed"
        );

        let (handle, _resolver) =
            crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(22, 0));
        assert_eq!(
            handle
                .type_name()
                .expect_err("type name must be fallible")
                .to_string(),
            "resolver failed"
        );
        assert!(!handle.is_reserved());
    }

    #[test]
    fn destroyed_handle_reports_destroyed_after_indirect_metadata_is_cleared() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(1));
        handle.disconnect();
        assert_eq!(handle.type_code().expect("type code"), 14, "ot_destroyed");
        assert_eq!(handle.type_name().expect("type name"), "destroyed");
        assert!(!handle.is_reserved());
    }

    #[test]
    fn null_resolved_indirect_handle_reports_null_not_a_distinct_missing_code() {
        // qpdf has no separate "missing" ot_* code — a dangling/broken
        // reference presents as ot_null, matching the qpdf null fallback's
        // documented is_null()==true contract.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Null);
        assert_eq!(handle.type_code().expect("type code"), 2, "ot_null");
        assert_eq!(handle.type_name().expect("type name"), "null");
        assert!(!handle.is_reserved());
    }

    #[test]
    fn resolved_indirect_handle_reports_its_real_value_type() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        assert_eq!(handle.type_code().expect("type code"), 4, "ot_integer");
        assert_eq!(handle.type_name().expect("type name"), "integer");
    }

    #[test]
    fn unparse_resolved_covers_every_direct_serializer_arm_for_a_nested_array_dict_and_stream() {
        // Keep this at a shallow, portable depth. The direct serializer
        // protects each recursive descent with stacker and exercises every
        // container arm (Array, Dictionary, Stream) without relying on the
        // legacy raw-object tree or its recursive Drop implementation.
        let stream = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        stream.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(
                b"Length".to_vec(),
                ObjectHandle::integer(0),
            )]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            filter_on_write: true,
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
            filter_on_write: true,
            stream_length: 0,
        });
        assert_eq!(handle.unparse(), b"9 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
    }

    #[test]
    fn direct_array_unparse_writes_indirect_children_as_references_not_recursed() {
        let (child, _resolver) = identity_tests::resolver_bearing_handle(ObjectValue::Integer(5));
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), child]);
        assert_eq!(array.unparse(), b"[ 1 20 0 R ]");
    }

    #[test]
    fn unparse_resolved_writes_an_unresolvable_indirect_array_child_as_a_reference() {
        // qpdf's array unparse resolves each element only to read the
        // element's OWN object/generation identity (`item.second->resolve();
        // auto og = item.second->getObjGen();`, `libqpdf/QPDF_Array.cc:
        // 130-132`) -- that identity is already known on the handle before
        // resolution ever runs, so qpdf never needs resolution to SUCCEED to
        // emit `N G R` for a dangling or otherwise unresolvable child. Drop
        // the child's resolver to force `try_dereference` to fail, matching
        // Codex's finding on PR #1354 that eagerly dereferencing before the
        // `object_ref()` check let a single unresolvable element fail the
        // whole array.
        let (child, resolver) = identity_tests::resolver_bearing_handle(ObjectValue::Integer(5));
        drop(resolver);
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), child]);

        assert_eq!(
            array
                .try_unparse_resolved()
                .expect("an unresolvable indirect child must not fail the whole array"),
            b"[ 1 20 0 R ]"
        );
    }

    #[test]
    fn a_direct_stream_value_unparse_resolved_inlines_rather_than_referencing() {
        // A *direct* Stream `ObjectValue` is a Rust-only construction shape
        // reachable through the public `ObjectHandle::stream` factory. qpdf
        // creates streams as indirect objects (`QPDF.cc:1912-1923`), so this
        // case has no qpdf output contract; retain the existing explicit
        // factory behavior without introducing another representation.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
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
        missing.set_resolved(ObjectValue::Null);
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), missing),
            (b"B".to_vec(), ObjectHandle::integer(1)),
        ]);
        assert_eq!(dict.unparse_resolved(), b"<< /B 1 >>");
    }

    #[test]
    fn unparse_resolved_keeps_a_resolved_non_null_indirect_dictionary_entry() {
        // qpdf resolves the child while checking nullness, then retains a
        // non-null indirect child as its reference form.
        let (unresolved, _resolver) =
            identity_tests::resolver_bearing_handle(ObjectValue::Integer(9));
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), unresolved)]);
        assert_eq!(dict.unparse_resolved(), b"<< /A 20 0 R >>");
    }

    #[test]
    fn unparse_resolved_resolves_an_unresolved_indirect_dictionary_child() {
        let (null_child, _resolver) = identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let dict = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), null_child.clone()),
            (b"B".to_vec(), ObjectHandle::integer(1)),
        ]);

        assert_eq!(dict.unparse_resolved(), b"<< /B 1 >>");
        assert!(null_child.is_resolved());
    }

    #[test]
    fn unparse_resolved_resolves_an_unresolved_top_level_handle() {
        let (handle, _resolver) = identity_tests::resolver_bearing_handle(ObjectValue::Integer(42));

        assert_eq!(handle.unparse_resolved(), b"42");
        assert!(handle.is_resolved());
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

    #[test]
    fn unparse_resolved_preserves_the_first_byte_of_a_slashless_dictionary_key() {
        let dictionary = ObjectHandle::dictionary(Vec::new());
        dictionary
            .replace_key(b"#A", ObjectHandle::integer(1))
            .expect("programmatic dictionary mutation");

        assert_eq!(dictionary.unparse_resolved(), b"<< #A 1 >>");
    }

    #[test]
    fn unparse_resolved_falls_back_but_try_unparse_resolved_reports_unresolved_values() {
        let handle = ObjectHandle::from_value(ObjectValue::Unresolved);

        assert_eq!(handle.unparse_resolved(), b"null");
        assert_eq!(
            handle
                .try_unparse_resolved()
                .expect_err("strict qpdf unparse must reject unresolved values")
                .to_string(),
            "attempted to unparse an unresolved QPDFObjectHandle"
        );
    }

    #[test]
    fn unparse_resolved_uses_the_canonical_form_for_an_unsafe_real_literal() {
        let handle = ObjectHandle::real_literal(0.4, b"not-a-real".to_vec());

        assert_eq!(handle.unparse_resolved(), b"0.4");
    }

    #[test]
    fn try_unparse_resolved_reports_missing_original_data_for_a_direct_stream() {
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(Vec::new()),
            stream_data: None,
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });

        assert!(handle
            .try_unparse_resolved()
            .expect_err("a direct original stream has no source")
            .to_string()
            .contains("pipeStreamData called for original direct stream"));
    }
}

#[cfg(test)]
mod unparse_object_tests {
    use super::identity_tests::{error_resolving_handle, resolver_bearing_handle};
    use super::*;
    use crate::writer::object::{dict_is_sig_with_byte_range, visible_dict_entries, write_child};
    use crate::writer::ObjectWriterEmission;

    fn compact_string_hook(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
        out.extend_from_slice(b"<hook:");
        out.extend_from_slice(value);
        out.extend_from_slice(b">");
        Ok(())
    }

    fn qdf_string_hook(out: &mut Vec<u8>, value: &[u8]) -> Result<()> {
        out.extend_from_slice(b"{hook:");
        out.extend_from_slice(value);
        out.extend_from_slice(b"}");
        Ok(())
    }

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
        dict.write_object(&mut out).unwrap();
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
        dict.write_object(&mut out).unwrap();
        assert_eq!(out, b"<< /ByteRange [ ] /Contents (hi) /Type /Page >>");
    }

    #[test]
    fn unparse_object_leaves_contents_literal_without_byte_range() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"Contents".to_vec(), ObjectHandle::string(b"hi".to_vec())),
        ]);
        let mut out = Vec::new();
        dict.write_object(&mut out).unwrap();
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
        dict.write_object(&mut out).unwrap();
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
        dict.write_object(&mut out).unwrap();
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
        let error = dict.write_object(&mut out).unwrap_err();
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
        dict.write_object_qdf(&mut out, 0).unwrap();
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
        let error = dict.write_object_qdf(&mut out, 0).unwrap_err();
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
        dict.write_stream_body(&mut out, false).unwrap();
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
        let error = dict.write_stream_body(&mut out, false).unwrap_err();
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
        ObjectHandle::integer(42).write_object(&mut out).unwrap();
        assert_eq!(out, b"42");
    }

    #[test]
    fn unparse_object_writes_a_boolean() {
        let mut out = Vec::new();
        ObjectHandle::boolean(true).write_object(&mut out).unwrap();
        assert_eq!(out, b"true");
        out.clear();
        ObjectHandle::boolean(false).write_object(&mut out).unwrap();
        assert_eq!(out, b"false");
    }

    #[test]
    fn unparse_object_writes_a_real() {
        let mut out = Vec::new();
        ObjectHandle::real(0.5).write_object(&mut out).unwrap();
        assert_eq!(out, b"0.5");
    }

    #[test]
    fn unparse_object_writes_a_string() {
        let mut out = Vec::new();
        ObjectHandle::string(b"hi".to_vec())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out, b"(hi)");
    }

    #[test]
    fn unparse_object_writes_an_operator_verbatim() {
        // ObjectValue::InlineImage shares the identical match arm and byte
        // path, so this one case covers both bindings.
        let mut out = Vec::new();
        ObjectHandle::operator(b"q".to_vec())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out, b"q");
    }

    #[test]
    fn unparse_object_serializes_large_scalar_payloads_without_deep_snapshotting() {
        let string_payload = vec![b's'; 256 * 1024];
        let mut out = Vec::new();
        ObjectHandle::string(string_payload.clone())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out.len(), string_payload.len() + 2);
        assert_eq!(out.first(), Some(&b'('));
        assert_eq!(out.last(), Some(&b')'));

        let operator_payload = vec![b'o'; 256 * 1024];
        out.clear();
        ObjectHandle::operator(operator_payload.clone())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out, operator_payload);

        let inline_image_payload = vec![b'i'; 256 * 1024];
        out.clear();
        ObjectHandle::inline_image(inline_image_payload.clone())
            .write_object_qdf(&mut out, 0)
            .unwrap();
        assert_eq!(out, inline_image_payload);
    }

    #[test]
    fn unparse_object_inlines_only_the_dictionary_of_a_direct_stream_value() {
        // A *direct* Stream ObjectValue has no qpdf counterpart (a real
        // QPDFObjectHandle's resolved value is never itself a stream
        // outside an indirect object), so there is no byte-parity oracle
        // here. This pins down the same "inline the dictionary, do not
        // write the `stream`/`endstream` framing" behavior `write_stream_body`
        // (Task 6) is separately responsible for, and stays consistent with
        // that primitive's scope rather than reproducing framing logic here.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        handle.write_object(&mut out).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary() {
        // Unlike the direct-stream case above, this *is* a real, reachable
        // qpdf shape: an indirect object whose resolved value is a stream.
        // `write_object`/`unparse_object_walk` dispatch on `self` directly
        // (never through `write_child`'s indirect-reference short-circuit,
        // which only applies to *child* positions during recursion), so
        // this reaches the same `ObjectValue::Stream` arm as the direct
        // case and inlines just the dictionary -- not qpdf's real
        // stream-writing output at this position (see
        // `ObjectHandle::write_object`'s own doc). Pins today's actual
        // behavior; `write_stream_body` (Task 6) is the primitive that
        // will implement the real stream-writing path.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.write_object(&mut out).unwrap();
        assert_eq!(out, b"<< /Length 2 >>");
    }

    #[test]
    fn unparse_object_writes_a_name_escaped() {
        let mut out = Vec::new();
        ObjectHandle::name(b"application/pdf".to_vec())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out, b"/application#2fpdf");
    }

    #[test]
    fn unparse_object_writes_a_real_literal_when_safe() {
        let mut out = Vec::new();
        ObjectHandle::real_literal(0.4, b".4".to_vec())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out, b".4");
    }

    #[test]
    fn unparse_object_falls_back_to_canonical_when_literal_is_unsafe() {
        let mut out = Vec::new();
        ObjectHandle::real_literal(0.4, b"nope".to_vec())
            .write_object(&mut out)
            .unwrap();
        assert_eq!(out, b"0.4");
    }

    #[test]
    fn unparse_object_writes_an_array_with_qpdf_spacing() {
        let handle = ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(2)]);
        let mut out = Vec::new();
        handle.write_object(&mut out).unwrap();
        assert_eq!(out, b"[ 1 2 ]");
    }

    #[test]
    fn unparse_object_writes_an_empty_array() {
        let mut out = Vec::new();
        ObjectHandle::array(vec![]).write_object(&mut out).unwrap();
        assert_eq!(out, b"[ ]");
    }

    #[test]
    fn unparse_object_writes_a_dict_and_suppresses_direct_null() {
        let handle = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"B".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        handle.write_object(&mut out).unwrap();
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
        handle.write_object(&mut out).unwrap();
        assert_eq!(out, b"<< /A 1 >>");
    }

    #[test]
    fn unparse_object_writes_an_empty_dict_when_every_entry_is_suppressed() {
        let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        let mut out = Vec::new();
        handle.write_object(&mut out).unwrap();
        assert_eq!(out, b"<< >>");
    }

    #[test]
    fn unparse_object_writes_a_retained_indirect_entry_as_reference_form() {
        let (indirect, _resolver) = resolver_bearing_handle(ObjectValue::Integer(7));
        let handle = ObjectHandle::dictionary(vec![(b"A".to_vec(), indirect)]);
        let mut out = Vec::new();
        handle.write_object(&mut out).unwrap();
        assert_eq!(out, b"<< /A 20 0 R >>");
    }

    #[test]
    fn unparse_object_propagates_a_dropped_document_error() {
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.write_object(&mut out).is_err());
    }

    #[test]
    fn unparse_encrypted_string_writer_replaces_direct_strings_only() {
        let (indirect, _resolver) =
            resolver_bearing_handle(ObjectValue::String(b"hidden".to_vec()));
        let handle = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::string(b"plain".to_vec())),
            (b"Indirect".to_vec(), indirect),
            (
                b"Nested".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::string(b"nested".to_vec())]),
            ),
        ]);
        let mut out = Vec::new();
        let mut callback = compact_string_hook;
        handle
            .write_object_with_string_writer(&mut out, &mut callback)
            .unwrap();
        assert_eq!(
            out,
            b"<< /A <hook:plain> /Indirect 20 0 R /Nested [ <hook:nested> ] >>"
        );
    }

    #[test]
    fn unparse_encrypted_string_writer_qdf_keeps_signature_contents_cleartext_hex() {
        let handle = ObjectHandle::dictionary(vec![
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (
                b"Contents".to_vec(),
                ObjectHandle::string(b"contents".to_vec()),
            ),
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        ]);
        let mut out = Vec::new();
        let mut callback = qdf_string_hook;
        handle
            .write_object_qdf_with_string_writer(&mut out, 4, &mut callback)
            .unwrap();
        assert_eq!(
            out,
            b"<<\n      /ByteRange [\n      ]\n      /Contents <636f6e74656e7473>\n      /Type /Sig\n    >>"
        );

        let mut compact = Vec::new();
        let mut compact_callback = compact_string_hook;
        handle
            .write_object_with_string_writer(&mut compact, &mut compact_callback)
            .unwrap();
        assert_eq!(
            compact,
            b"<< /ByteRange [ ] /Contents <636f6e74656e7473> /Type /Sig >>"
        );

        let mut stream_compact = Vec::new();
        let mut stream_compact_callback = compact_string_hook;
        handle
            .write_stream_body_with_string_writer(
                &mut stream_compact,
                false,
                &mut stream_compact_callback,
            )
            .unwrap();
        assert_eq!(
            stream_compact,
            b"<< /ByteRange [ ] /Contents <636f6e74656e7473> /Type /Sig >>"
        );

        let mut stream_qdf = Vec::new();
        let mut stream_qdf_callback = qdf_string_hook;
        handle
            .write_stream_body_qdf_with_string_writer(&mut stream_qdf, 0, &mut stream_qdf_callback)
            .unwrap();
        assert_eq!(
            stream_qdf,
            b"<<\n  /ByteRange [\n  ]\n  /Contents <636f6e74656e7473>\n  /Type /Sig\n>>"
        );
    }

    #[test]
    fn unparse_encrypted_string_writer_stream_bodies_keep_qpdf_layout() {
        let dict = ObjectHandle::dictionary(vec![
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"Name".to_vec(),
                    ObjectHandle::string(b"params".to_vec()),
                )]),
            ),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let mut compact = Vec::new();
        let mut compact_callback = compact_string_hook;
        dict.write_stream_body_with_string_writer(&mut compact, false, &mut compact_callback)
            .unwrap();
        assert_eq!(
            compact,
            b"<< /DecodeParms << /Name <hook:params> >> /Filter /FlateDecode /Length 3 >>"
        );

        let mut qdf = Vec::new();
        let mut qdf_callback = qdf_string_hook;
        dict.write_stream_body_qdf_with_string_writer(&mut qdf, 2, &mut qdf_callback)
            .unwrap();
        assert_eq!(
            qdf,
            b"<<\n    /DecodeParms <<\n      /Name {hook:params}\n    >>\n    /Filter /FlateDecode\n    /Length 3\n  >>"
        );
    }

    #[test]
    fn unparse_string_writer_covers_stream_reserved_refiltered_and_special_children() {
        let stream_dict = ObjectHandle::dictionary(vec![
            (
                b"DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"Name".to_vec(),
                    ObjectHandle::string(b"params".to_vec()),
                )]),
            ),
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"Label".to_vec(), ObjectHandle::string(b"stream".to_vec())),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);
        let stream = ObjectHandle::stream(stream_dict.clone(), Rc::new(b"raw".to_vec()));
        let mut compact = Vec::new();
        let mut compact_callback = compact_string_hook;
        stream
            .write_object_with_string_writer(&mut compact, &mut compact_callback)
            .expect("direct stream object callback emission");
        assert!(compact
            .windows(b"<hook:stream>".len())
            .any(|part| part == b"<hook:stream>"));

        let mut qdf = Vec::new();
        let mut qdf_callback = qdf_string_hook;
        stream
            .write_object_qdf_with_string_writer(&mut qdf, 2, &mut qdf_callback)
            .expect("direct stream object QDF callback emission");
        assert!(qdf
            .windows(b"{hook:stream}".len())
            .any(|part| part == b"{hook:stream}"));

        let mut stream_body = Vec::new();
        let mut stream_body_callback = compact_string_hook;
        stream
            .write_stream_body_with_string_writer(
                &mut stream_body,
                false,
                &mut stream_body_callback,
            )
            .expect("direct stream compact body callback emission");
        assert!(stream_body
            .windows(b"<hook:stream>".len())
            .any(|part| part == b"<hook:stream>"));

        let mut refiltered = Vec::new();
        let mut refiltered_callback = compact_string_hook;
        stream_dict
            .write_stream_body_with_string_writer(&mut refiltered, true, &mut refiltered_callback)
            .expect("refiltered stream dictionary callback emission");
        assert!(!refiltered
            .windows(b"DecodeParms".len())
            .any(|part| part == b"DecodeParms"));
        assert!(refiltered
            .windows(b"/Filter /FlateDecode".len())
            .any(|part| part == b"/Filter /FlateDecode"));

        let mut stream_qdf = Vec::new();
        let mut stream_qdf_callback = qdf_string_hook;
        stream
            .write_stream_body_qdf_with_string_writer(&mut stream_qdf, 1, &mut stream_qdf_callback)
            .expect("stream value QDF body callback emission");
        assert!(stream_qdf
            .windows(b"{hook:stream}".len())
            .any(|part| part == b"{hook:stream}"));

        let scalar_stream = ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(Vec::new()));
        let mut scalar_stream_body = Vec::new();
        let mut scalar_stream_callback = compact_string_hook;
        scalar_stream
            .write_stream_body_with_string_writer(
                &mut scalar_stream_body,
                false,
                &mut scalar_stream_callback,
            )
            .expect("stream with a non-dictionary dictionary handle uses an empty body");
        assert_eq!(scalar_stream_body, b"<< >>");
        let mut scalar_stream_qdf_body = Vec::new();
        let mut scalar_stream_qdf_callback = qdf_string_hook;
        scalar_stream
            .write_stream_body_qdf_with_string_writer(
                &mut scalar_stream_qdf_body,
                1,
                &mut scalar_stream_qdf_callback,
            )
            .expect("QDF stream with a non-dictionary dictionary handle uses an empty body");
        assert_eq!(scalar_stream_qdf_body, b"<<\n >>");

        let mut scalar_body = Vec::new();
        let mut scalar_callback = compact_string_hook;
        ObjectHandle::integer(1)
            .write_stream_body_with_string_writer(&mut scalar_body, false, &mut scalar_callback)
            .expect("non-dictionary stream body degrades to an empty dictionary");
        assert_eq!(scalar_body, b"<< >>");
        let mut scalar_qdf_body = Vec::new();
        let mut scalar_qdf_callback = qdf_string_hook;
        ObjectHandle::integer(1)
            .write_stream_body_qdf_with_string_writer(
                &mut scalar_qdf_body,
                1,
                &mut scalar_qdf_callback,
            )
            .expect("non-dictionary QDF stream body degrades to an empty dictionary");
        assert_eq!(scalar_qdf_body, b"<<\n >>");

        let reserved = ObjectHandle::new_reserved_direct();
        let mut reserved_out = Vec::new();
        let mut reserved_callback = compact_string_hook;
        assert!(reserved
            .write_object_with_string_writer(&mut reserved_out, &mut reserved_callback)
            .is_err());
        assert!(reserved
            .write_object_qdf_with_string_writer(&mut reserved_out, 0, &mut reserved_callback,)
            .is_err());
        assert!(reserved
            .write_stream_body_with_string_writer(&mut reserved_out, false, &mut reserved_callback,)
            .is_err());
        assert!(reserved
            .write_stream_body_qdf_with_string_writer(&mut reserved_out, 0, &mut reserved_callback,)
            .is_err());

        let non_string_sig = ObjectHandle::dictionary(vec![
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), ObjectHandle::integer(7)),
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        ]);
        let mut non_string_sig_out = Vec::new();
        let mut non_string_sig_callback = compact_string_hook;
        non_string_sig
            .write_object_with_string_writer(&mut non_string_sig_out, &mut non_string_sig_callback)
            .expect("non-string signature contents use the ordinary child writer");
        assert!(non_string_sig_out
            .windows(b"/Contents 7".len())
            .any(|part| part == b"/Contents 7"));

        let (indirect_contents, _resolver) =
            resolver_bearing_handle(ObjectValue::String(b"indirect".to_vec()));
        let indirect_sig = ObjectHandle::dictionary(vec![
            (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
            (b"Contents".to_vec(), indirect_contents),
            (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        ]);
        let mut indirect_sig_out = Vec::new();
        let mut indirect_sig_callback = qdf_string_hook;
        indirect_sig
            .write_object_qdf_with_string_writer(
                &mut indirect_sig_out,
                0,
                &mut indirect_sig_callback,
            )
            .expect("indirect signature contents use the reference writer");
        assert!(indirect_sig_out
            .windows(b"/Contents 20 0 R".len())
            .any(|part| part == b"/Contents 20 0 R"));
    }

    #[test]
    fn unparse_object_qdf_writes_a_scalar_like_plain_unparse() {
        let mut out = Vec::new();
        ObjectHandle::integer(42)
            .write_object_qdf(&mut out, 0)
            .unwrap();
        assert_eq!(out, b"42");
    }

    #[test]
    fn unparse_object_qdf_writes_an_array_with_newline_indent() {
        let handle = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let mut out = Vec::new();
        handle.write_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"[\n  1\n]");
    }

    #[test]
    fn unparse_object_qdf_writes_a_dict_with_newline_indent_and_suppresses_null() {
        let handle = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"B".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        handle.write_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /A 1\n>>");
    }

    #[test]
    fn unparse_object_qdf_nests_indent_one_level_deeper() {
        let handle = ObjectHandle::dictionary(vec![(
            b"Kids".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(1)]),
        )]);
        let mut out = Vec::new();
        handle.write_object_qdf(&mut out, 0).unwrap();
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
        handle.write_object_qdf(&mut out, 0).unwrap();
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
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.write_object_qdf(&mut out, 0).unwrap();
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
        handle.write_object_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n  /D <<\n    /A 1\n  >>\n>>");
    }

    #[test]
    fn unparse_object_qdf_propagates_a_dropped_document_error() {
        let (indirect, resolver) = resolver_bearing_handle(ObjectValue::Null);
        drop(resolver);
        let mut out = Vec::new();
        assert!(indirect.write_object_qdf(&mut out, 0).is_err());
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
        handle.write_object_qdf(&mut out, 4).unwrap();
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
        handle.write_object_qdf(&mut out, 4).unwrap();
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
        dict.write_stream_body(&mut out, false).unwrap();
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
        dict.write_stream_body(&mut out, true).unwrap();
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
        dict.write_stream_body(&mut out, true).unwrap();
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
        assert!(dict.write_stream_body(&mut out, false).is_err());
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
            .write_object_with_ref_map_and_removed(&mut object, &map, &BTreeSet::new())
            .unwrap();
        assert_eq!(object, b"<< /Child 8 0 R /Length 2 >>");

        let mut body = Vec::new();
        stream
            .write_stream_body_with_ref_map_and_removed(&mut body, false, &map, &BTreeSet::new())
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
            .write_stream_body_with_ref_map_and_removed(
                &mut signature_body,
                false,
                &map,
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(
            signature_body,
            b"<< /ByteRange [ ] /Contents <6869> /Type /Sig >>"
        );

        let mut nested_non_dictionary = Vec::new();
        ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()))
            .write_stream_body_with_ref_map_and_removed(
                &mut nested_non_dictionary,
                false,
                &map,
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(nested_non_dictionary, b"<< >>");

        let mut non_dictionary = Vec::new();
        ObjectHandle::integer(5)
            .write_stream_body_with_ref_map_and_removed(
                &mut non_dictionary,
                false,
                &map,
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(non_dictionary, b"<< >>");
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
        dict.write_object_with_ref_map_and_removed(&mut object, &map, &removed_refs)
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
            .write_stream_body_with_ref_map_and_removed(&mut body, false, &map, &removed_refs)
            .unwrap();
        assert_eq!(body, b"<< /Mapped 8 0 R /Length 2 >>");
    }

    #[test]
    fn mapped_stream_writers_cover_qdf_length_and_filter_variants() {
        let kept = ObjectHandle::new_indirect_unresolved(ObjectRef::new(30, 0), 0);
        kept.set_resolved(ObjectValue::Integer(7));
        let removed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(31, 0), 0);
        removed.set_resolved(ObjectValue::Integer(8));
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![
                (b"ByteRange".to_vec(), ObjectHandle::array(vec![])),
                (b"Contents".to_vec(), ObjectHandle::string(b"sig".to_vec())),
                (b"DecodeParms".to_vec(), ObjectHandle::dictionary(vec![])),
                (
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                ),
                (b"Length".to_vec(), ObjectHandle::integer(3)),
                (b"Link".to_vec(), kept),
                (b"Removed".to_vec(), removed),
                (b"Text".to_vec(), ObjectHandle::string(b"plain".to_vec())),
                (b"Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            ]),
            stream_data: Some(Rc::new(b"abc".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 3,
        });
        let mut removed_refs = BTreeSet::new();
        removed_refs.insert(ObjectRef::new(31, 0));
        let map = |object_ref: ObjectRef| {
            Ok(ObjectRef::new(
                object_ref.number + 100,
                object_ref.generation,
            ))
        };

        let mut compact = Vec::new();
        stream
            .write_stream_body_with_ref_map_and_removed_with_string_writer(
                &mut compact,
                false,
                &map,
                &removed_refs,
                &mut compact_string_hook,
            )
            .unwrap();
        let compact_text = String::from_utf8_lossy(&compact);
        assert!(compact_text.contains("/Link 130 0 R"));
        assert!(compact_text.contains("/Length 3"));
        assert!(compact_text.contains("<hook:plain>"));
        assert!(!compact_text.contains("/Removed"));

        let mut refiltered = Vec::new();
        stream
            .write_stream_body_with_ref_map_and_removed_with_string_writer(
                &mut refiltered,
                true,
                &map,
                &removed_refs,
                &mut compact_string_hook,
            )
            .unwrap();
        let refiltered_text = String::from_utf8_lossy(&refiltered);
        assert!(!refiltered_text.contains("/Filter /ASCIIHexDecode"));
        assert!(!refiltered_text.contains("/DecodeParms"));
        assert!(refiltered_text.contains("/Filter /FlateDecode"));

        let mut qdf_source_length = Vec::new();
        stream
            .write_stream_body_qdf_with_ref_map_and_removed_and_length(
                &mut qdf_source_length,
                2,
                &map,
                &removed_refs,
                None,
            )
            .unwrap();
        let qdf_source_text = String::from_utf8_lossy(&qdf_source_length);
        assert!(qdf_source_text.contains("/Length 3"));
        assert!(qdf_source_text.contains("/Link 130 0 R"));

        let mut qdf_synthetic_length = Vec::new();
        stream
            .write_stream_body_qdf_with_ref_map_and_removed_and_length(
                &mut qdf_synthetic_length,
                2,
                &map,
                &removed_refs,
                Some(ObjectRef::new(77, 0)),
            )
            .unwrap();
        assert!(String::from_utf8_lossy(&qdf_synthetic_length).contains("/Length 77 0 R"));

        let mut qdf_encrypted_source_length = Vec::new();
        stream
            .write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                &mut qdf_encrypted_source_length,
                2,
                &map,
                &removed_refs,
                None,
                &mut qdf_string_hook,
            )
            .unwrap();
        let qdf_encrypted_source_text = String::from_utf8_lossy(&qdf_encrypted_source_length);
        assert!(qdf_encrypted_source_text.contains("{hook:plain}"));
        assert!(qdf_encrypted_source_text.contains("/Length 3"));

        let mut qdf_encrypted_synthetic_length = Vec::new();
        stream
            .write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                &mut qdf_encrypted_synthetic_length,
                2,
                &map,
                &removed_refs,
                Some(ObjectRef::new(78, 0)),
                &mut qdf_string_hook,
            )
            .unwrap();
        assert!(String::from_utf8_lossy(&qdf_encrypted_synthetic_length).contains("/Length 78 0 R"));

        let scalar = ObjectHandle::integer(5);
        let mut scalar_qdf = Vec::new();
        scalar
            .write_stream_body_qdf_with_ref_map_and_removed_and_length(
                &mut scalar_qdf,
                0,
                &map,
                &removed_refs,
                None,
            )
            .unwrap();
        assert_eq!(scalar_qdf, b"<<\n>>");
        let mut scalar_qdf_encrypted = Vec::new();
        scalar
            .write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                &mut scalar_qdf_encrypted,
                0,
                &map,
                &removed_refs,
                None,
                &mut qdf_string_hook,
            )
            .unwrap();
        assert_eq!(scalar_qdf_encrypted, b"<<\n  /Length null\n>>");
        let mut scalar_compact_encrypted = Vec::new();
        scalar
            .write_stream_body_with_ref_map_and_removed_with_string_writer(
                &mut scalar_compact_encrypted,
                false,
                &map,
                &removed_refs,
                &mut compact_string_hook,
            )
            .unwrap();
        assert_eq!(scalar_compact_encrypted, b"<< >>");

        let no_length = ObjectHandle::dictionary(vec![(
            b"Text".to_vec(),
            ObjectHandle::string(b"no-length".to_vec()),
        )]);
        let mut no_length_qdf = Vec::new();
        no_length
            .write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                &mut no_length_qdf,
                0,
                &map,
                &removed_refs,
                None,
                &mut qdf_string_hook,
            )
            .unwrap();
        assert!(String::from_utf8_lossy(&no_length_qdf).contains("/Length null"));

        let reserved = ObjectHandle::new_reserved_direct();
        let mut reserved_out = Vec::new();
        assert!(reserved
            .write_stream_body_qdf_with_ref_map_and_removed_and_length(
                &mut reserved_out,
                0,
                &map,
                &removed_refs,
                None,
            )
            .is_err());
        assert!(reserved
            .write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                &mut reserved_out,
                0,
                &map,
                &removed_refs,
                None,
                &mut qdf_string_hook,
            )
            .is_err());
        assert!(reserved
            .write_stream_body_with_ref_map_and_removed_with_string_writer(
                &mut reserved_out,
                false,
                &map,
                &removed_refs,
                &mut compact_string_hook,
            )
            .is_err());

        let stream_with_non_dictionary_dict =
            ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(Vec::new()));
        let mut non_dictionary_dict_qdf = Vec::new();
        stream_with_non_dictionary_dict
            .write_stream_body_qdf_with_ref_map_and_removed_and_length(
                &mut non_dictionary_dict_qdf,
                0,
                &map,
                &removed_refs,
                None,
            )
            .unwrap();
        assert_eq!(non_dictionary_dict_qdf, b"<<\n>>");
        let mut non_dictionary_dict_qdf_string = Vec::new();
        stream_with_non_dictionary_dict
            .write_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(
                &mut non_dictionary_dict_qdf_string,
                0,
                &map,
                &removed_refs,
                None,
                &mut qdf_string_hook,
            )
            .unwrap();
        assert_eq!(non_dictionary_dict_qdf_string, b"<<\n  /Length null\n>>");
        let mut non_dictionary_dict_compact_string = Vec::new();
        stream_with_non_dictionary_dict
            .write_stream_body_with_ref_map_and_removed_with_string_writer(
                &mut non_dictionary_dict_compact_string,
                false,
                &map,
                &removed_refs,
                &mut compact_string_hook,
            )
            .unwrap();
        assert_eq!(non_dictionary_dict_compact_string, b"<< >>");
    }

    #[test]
    fn unparse_stream_body_suppresses_a_null_valued_key() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Length".to_vec(), ObjectHandle::integer(3)),
            (b"Metadata".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        dict.write_stream_body(&mut out, false).unwrap();
        assert_eq!(out, b"<< /Length 3 >>");
    }

    #[test]
    fn unparse_stream_body_uses_the_dictionary_of_a_direct_stream_value() {
        // Mirrors unparse_object_inlines_only_the_dictionary_of_a_direct_stream_value
        // above: a *direct* Stream ObjectValue has no qpdf counterpart (a
        // real QPDFObjectHandle's resolved value is never itself a stream
        // outside an indirect object), but `write_stream_body` must still
        // use its `stream_dict`'s entries rather than falling into the
        // non-dictionary-self `<< >>` degrade below -- keeping the promise
        // those two `write_object`/`write_object_qdf` tests made on this
        // primitive's behalf ("`write_stream_body` (Task 6) is separately
        // responsible for this").
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(2))]);
        let handle = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict,
            stream_data: Some(Rc::new(b"ab".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        handle.write_stream_body(&mut out, false).unwrap();
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
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.write_stream_body(&mut out, false).unwrap();
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
        handle.write_stream_body(&mut out, false).unwrap();
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
        assert!(handle.write_stream_body(&mut out, false).is_err());
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
        handle.write_stream_body(&mut out, false).unwrap();
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
            .write_stream_body(&mut out, false)
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
        assert!(indirect.write_stream_body(&mut out, false).is_err());
    }

    // QDF-mode sibling suite of the `unparse_stream_body_*` tests above,
    // for `write_stream_body_qdf`. Every hardcoded expected byte string
    // below was cross-checked against a live call to
    // `Dictionary::write_pdf_stream_qdf` (`object.rs`) with an equivalent
    // dictionary before being pinned here, not hand-derived from reading
    // the algorithm alone -- see this primitive's own doc for the full
    // qpdf-correspondence and the deliberate absence of a `refiltered`
    // parameter.

    #[test]
    fn unparse_stream_body_qdf_writes_length_last_preserved() {
        // No `refiltered` dimension exists for the QDF shape (see
        // `write_stream_body_qdf`'s own doc for why), so unlike its
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
        dict.write_stream_body_qdf(&mut out, 0).unwrap();
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
        dict.write_stream_body_qdf(&mut out, 0).unwrap();
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
        let error = dict.write_stream_body_qdf(&mut out, 0).unwrap_err();
        assert_eq!(error.to_string(), "object 99 0 belongs to a dropped PDF");
    }

    #[test]
    fn unparse_stream_body_qdf_suppresses_a_null_valued_key() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Length".to_vec(), ObjectHandle::integer(3)),
            (b"Metadata".to_vec(), ObjectHandle::null()),
        ]);
        let mut out = Vec::new();
        dict.write_stream_body_qdf(&mut out, 0).unwrap();
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
        dict.write_stream_body_qdf(&mut out, 0).unwrap();
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
        dict.write_stream_body_qdf(&mut out, 4).unwrap();
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
        dict.write_stream_body_qdf(&mut out, 4).unwrap();
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
        dict.write_stream_body_qdf(&mut out, 0).unwrap();
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
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        handle.write_stream_body_qdf(&mut out, 0).unwrap();
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
            filter_on_write: true,
            stream_length: 0,
        });
        let mut out = Vec::new();
        indirect.write_stream_body_qdf(&mut out, 0).unwrap();
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
        handle.write_stream_body_qdf(&mut out, 0).unwrap();
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
        assert!(handle.write_stream_body_qdf(&mut out, 0).is_err());
    }

    #[test]
    fn unparse_stream_body_qdf_writes_empty_dict_when_stream_dict_is_not_a_dictionary() {
        // Mirrors unparse_stream_body_writes_empty_dict_when_stream_dict_is_not_a_dictionary:
        // `stream_dict` is itself typed as an `ObjectHandle`, so nothing at
        // the type level prevents it from resolving to something other than
        // a `Dictionary`.
        let handle = ObjectHandle::stream(ObjectHandle::integer(5), Rc::new(b"ab".to_vec()));
        let mut out = Vec::new();
        handle.write_stream_body_qdf(&mut out, 0).unwrap();
        assert_eq!(out, b"<<\n>>");
    }

    #[test]
    fn unparse_stream_body_qdf_writes_empty_dict_for_a_non_dictionary_self() {
        // Mirrors unparse_stream_body_writes_empty_dict_for_a_non_dictionary_self:
        // pins the doc comment's typed-input-assumption claim.
        let mut out = Vec::new();
        ObjectHandle::integer(5)
            .write_stream_body_qdf(&mut out, 0)
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
        assert!(indirect.write_stream_body_qdf(&mut out, 0).is_err());
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
        dict.write_trailer(&mut out, false, None).unwrap();
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
        dict.write_trailer(&mut out, true, None).unwrap();
        assert!(!String::from_utf8_lossy(&out).contains("<<"));
        assert!(String::from_utf8_lossy(&out).ends_with(">>"));
    }

    #[test]
    fn unparse_trailer_without_id_or_encrypt_omits_both() {
        let dict = ObjectHandle::dictionary(vec![(b"Size".to_vec(), ObjectHandle::integer(9))]);
        let mut out = Vec::new();
        dict.write_trailer(&mut out, false, None).unwrap();
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
        dict.write_trailer(&mut out, false, None).unwrap();
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
        dict.write_trailer(&mut out, false, Some(&mut id_writer))
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
        dict.write_trailer(&mut out, false, None).unwrap();
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
        dict.write_trailer(&mut out, false, None).unwrap();
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
        dict.write_trailer(&mut out, false, None).unwrap();
        assert_eq!(out, b"trailer << /ID [ 1 <8f> ] >>");
    }

    #[test]
    fn unparse_trailer_id_writer_none_falls_back_for_scalar_id() {
        // Mirrors write_id_style_value_falls_back_for_unexpected_shapes
        // (object.rs): a non-array /ID value is delegated to write_child
        // verbatim rather than being routed through the compact-pair path.
        let dict = ObjectHandle::dictionary(vec![(b"ID".to_vec(), ObjectHandle::integer(7))]);
        let mut out = Vec::new();
        dict.write_trailer(&mut out, false, None).unwrap();
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
        dict.write_trailer(&mut out, false, None).unwrap();
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
            .write_trailer(&mut out, false, None)
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
        indirect.write_trailer(&mut out, false, None).unwrap();
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
        assert!(indirect.write_trailer(&mut out, false, None).is_err());
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::*;

    #[test]
    fn qpdf_exception_what_formats_object_and_offset_fields() {
        assert_eq!(
            format_qpdf_exception_what("input.pdf", "object 4 0", 28, "detail"),
            "input.pdf (object 4 0, offset 28): detail"
        );
        assert_eq!(
            format_qpdf_exception_what("", "object 4 0", -1, "detail"),
            "object 4 0: detail"
        );
    }

    #[test]
    fn qpdf_exception_what_matches_createwhat_for_a_negative_offset_with_no_object() {
        // qpdf's createWhat only skips the parenthesized segment when
        // `object` is empty AND `offset == 0` -- a negative offset with an
        // empty object still enters that branch and emits an empty `()`
        // (QPDFExc.cc:16-48). Regression for a prior `offset <= 0` guard
        // that instead skipped the parentheses entirely for any
        // non-positive offset.
        assert_eq!(
            format_qpdf_exception_what("input.pdf", "", -1, "detail"),
            "input.pdf (): detail"
        );
        assert_eq!(
            format_qpdf_exception_what("input.pdf", "", 0, "detail"),
            "input.pdf: detail"
        );
    }

    #[test]
    fn make_direct_rebinds_only_the_receiver_and_isolates_repeated_indirect_children() {
        let shared_array = ObjectHandle::new_indirect_unresolved(ObjectRef::new(11, 0), -1);
        shared_array.set_resolved(ObjectValue::Array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::integer(2),
            ObjectHandle::integer(3),
        ]));
        let original = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), shared_array.clone()),
            (
                b"B".to_vec(),
                ObjectHandle::dictionary(vec![(b"A".to_vec(), shared_array.clone())]),
            ),
        ]);
        let original_alias = original.clone();
        let mut direct = original;

        direct.make_direct(false).expect("direct conversion");

        assert!(original_alias.as_dictionary().is_some());
        assert!(original_alias.get_key(b"/A").is_indirect());
        let first = direct.get_key(b"/A");
        let second = direct.get_key(b"/B").get_key(b"/A");
        assert!(first.is_direct());
        assert!(second.is_direct());
        assert!(!first.is_same_object_as(&second));

        first
            .set_array_item(1, ObjectHandle::integer(5))
            .expect("mutate first copied array");
        assert_eq!(
            first.try_array_item(1).unwrap().unwrap().as_integer(),
            Some(5)
        );
        assert_eq!(
            second.try_array_item(1).unwrap().unwrap().as_integer(),
            Some(2)
        );
    }

    #[test]
    fn make_direct_stops_at_streams_only_when_allowed() {
        let stream = ObjectHandle::new_indirect_unresolved(ObjectRef::new(12, 0), -1);
        stream.set_resolved(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![]),
            stream_data: Some(Rc::new(b"salad".to_vec())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        let original = ObjectHandle::dictionary(vec![(b"Stream".to_vec(), stream.clone())]);

        let mut rejects_stream = original.clone();
        let error = rejects_stream
            .make_direct(false)
            .expect_err("makeDirect must reject a stream without allow_streams");
        assert!(matches!(
            error,
            Error::System(message)
                if message == "attempt to make a stream into a direct object"
        ));
        assert!(rejects_stream.get_key(b"/Stream").is_indirect());

        let mut stops_at_stream = original;
        stops_at_stream
            .make_direct(true)
            .expect("allow_streams must preserve the stream reference");
        assert!(stops_at_stream.get_key(b"/Stream").is_indirect());
        assert!(stops_at_stream
            .get_key(b"/Stream")
            .is_same_object_as(&stream));
    }

    #[test]
    fn make_direct_reports_an_indirect_cycle_without_rebinding_the_receiver() {
        let first = ObjectHandle::new_indirect_unresolved(ObjectRef::new(8, 0), -1);
        let second = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        first.set_resolved(ObjectValue::Dictionary(
            [(b"/A".to_vec(), second.clone())].into_iter().collect(),
        ));
        second.set_resolved(ObjectValue::Array(vec![first.clone()]));
        let alias = first.clone();
        let mut candidate = first;

        let error = candidate
            .make_direct(false)
            .expect_err("recursive indirect graph must be rejected");
        assert!(matches!(
            error,
            Error::System(message)
                if message == "loop detected while converting object from indirect to direct"
        ));
        assert!(candidate.is_indirect());
        assert!(candidate.is_same_object_as(&alias));
    }

    #[test]
    fn make_direct_rejects_reserved_and_non_pdf_object_values() {
        let mut reserved = ObjectHandle::new_reserved_direct();
        let reserved_error = reserved
            .make_direct(false)
            .expect_err("reserved handles cannot become direct values");
        assert!(matches!(
            reserved_error,
            Error::System(message)
                if message == "QPDFObjectHandle: attempting to make a reserved object handle direct"
        ));

        let mut operator = ObjectHandle::operator(b"q".to_vec());
        let operator_error = operator
            .make_direct(false)
            .expect_err("content operators are not PDF object values");
        assert!(matches!(
            operator_error,
            Error::System(message)
                if message == "QPDFObjectHandle::makeDirectInternal: unknown object type"
        ));
    }

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

        fn warn_stream_data(
            &self,
            offset: u64,
            _description_override: Option<&str>,
            message: String,
        ) -> crate::Result<()> {
            self.warnings
                .borrow_mut()
                .push(format!("offset {offset}: {message}"));
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
                filter_on_write: true,
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
                filter_on_write: true,
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
                filter_on_write: true,
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
            Error::Unsupported(message)
                if message == "offset 9: getStreamData called on unfilterable stream"
        ));
        assert_eq!(resolver.calls.borrow().as_slice(), &[(false, false)]);
    }

    #[test]
    fn get_stream_data_rejects_filters_gated_by_the_requested_decode_level() {
        for (filter, decode_level) in [
            (b"DCTDecode".as_slice(), DecodeLevel::Generalized),
            (b"RunLengthDecode".as_slice(), DecodeLevel::Generalized),
        ] {
            let stream = ObjectHandle::stream(
                ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::name(filter.to_vec()),
                )]),
                Rc::new(b"raw filtered bytes".to_vec()),
            );

            let error = stream
                .get_stream_data(decode_level)
                .expect_err("gated raw bytes must not be labeled as decoded");

            assert!(matches!(
                error,
                Error::Unsupported(message)
                    if message == "getStreamData called on unfilterable stream"
            ));
        }
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
                filter_on_write: true,
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
                filter_on_write: true,
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
                filter_on_write: true,
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
    fn pipe_stream_data_reports_flate_warnings_through_stream_data_sink() {
        let raw = vec![0x78];
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![(
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                )]),
                stream_data: None,
                stream_provider: None,
                filter_on_write: true,
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
            &["offset 9: input stream is complete but output may still be valid"]
        );

        resolver.warnings.borrow_mut().clear();
        let mut suppressed_sink = crate::pipeline::buffer::Buffer::new("suppressed", None);
        assert!(stream
            .pipe_stream_data(
                &mut suppressed_sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::Generalized,
                true,
                false,
            )
            .unwrap());
        assert!(filtering_attempted);
        assert!(suppressed_sink.take_buffer().unwrap().is_empty());
        assert!(resolver.warnings.borrow().is_empty());
    }

    #[test]
    fn pipe_stream_data_reports_normalizer_warnings_only_after_source_success() {
        let raw = b"<0g".to_vec();
        let resolver = Rc::new(SourcePipeResolver {
            value: ObjectValue::Stream {
                stream_dict: ObjectHandle::dictionary(vec![]),
                stream_data: None,
                stream_provider: None,
                filter_on_write: true,
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
                "offset 9: content normalization encountered bad tokens",
                "offset 9: normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents",
                "offset 9: Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.",
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
    fn pipe_stream_data_normalizer_warnings_use_the_stream_data_offset() {
        let mut pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf
            .new_stream_with_data(Rc::new(b"<0g".to_vec()))
            .expect("stream");
        stream.reset_parsed_offset();
        stream.set_parsed_offset_if_unset(9);
        pdf.set_suppress_warnings(true);

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
            .expect("normalize stream"));

        let diagnostics = pdf.repair_diagnostics();
        assert_eq!(diagnostics.entries().len(), 3);
        let offsets: Vec<_> = diagnostics
            .entries()
            .iter()
            .map(|diagnostic| diagnostic.offset)
            .collect();
        assert_eq!(offsets, vec![Some(9); 3]);
    }

    #[test]
    fn pipe_stream_data_programmatic_normalizer_warnings_use_object_fallback() {
        let mut pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf
            .new_stream_with_data(Rc::new(b"<0g".to_vec()))
            .expect("stream");
        stream.reset_parsed_offset();
        pdf.set_suppress_warnings(true);

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
            .expect("normalize programmatic stream"));

        let diagnostics = pdf.repair_diagnostics();
        assert_eq!(diagnostics.entries().len(), 3);
        assert!(diagnostics
            .entries()
            .iter()
            .all(|diagnostic| diagnostic.offset.is_none()));
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
                filter_on_write: true,
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
                filter_on_write: true,
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
    fn try_get_key_on_a_non_dictionary_handle_reports_qpdf_type_warning() {
        let scalar = ObjectHandle::integer(5);
        assert!(matches!(
            scalar.try_get_key(b"/A"),
            Err(crate::Error::System(message))
                if message == "operation for dictionary attempted on object of type integer: returning null for attempted key retrieval"
        ));
    }

    #[test]
    fn replace_key_mutates_the_live_dictionary_in_place() {
        let dict = ObjectHandle::dictionary(vec![]);
        let clone = dict.clone();
        dict.replace_key(b"/A", ObjectHandle::integer(9)).unwrap();
        assert_eq!(clone.get_key(b"/A").as_integer(), Some(9));
    }

    #[test]
    fn replace_key_overwrites_an_existing_key() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        dict.replace_key(b"/A", ObjectHandle::integer(2)).unwrap();
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

        dict.replace_key(b"/A", ObjectHandle::null()).unwrap();

        assert!(!dict.has_key(b"/A"));
        assert!(child.containing_object_refs().is_empty());
    }

    #[test]
    fn replace_key_with_direct_null_keeps_a_missing_key_absent() {
        let dict = ObjectHandle::dictionary(vec![]);

        dict.replace_key(b"/Missing", ObjectHandle::null()).unwrap();

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

        dict.replace_key(b"/A", ObjectHandle::null()).unwrap();

        assert!(!dict.has_key(b"/A"));
    }

    #[test]
    fn replace_key_with_direct_null_on_a_non_dictionary_reports_qpdf_type_warning() {
        let scalar = ObjectHandle::integer(1);

        let error = scalar
            .replace_key(b"/A", ObjectHandle::null())
            .expect_err("a contextless qpdf type warning is an exception");

        assert!(matches!(
            error,
            Error::System(message)
                if message == "operation for dictionary attempted on object of type integer: ignoring key replacement request"
        ));
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn replace_key_preserves_a_resolved_indirect_null_and_its_identity() {
        let indirect_null = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        indirect_null.set_resolved(ObjectValue::Null);
        let dict = ObjectHandle::dictionary(vec![]);

        dict.replace_key(b"/Null", indirect_null.clone()).unwrap();

        let retained = dict.get_key(b"/Null");
        assert!(!dict.has_key(b"/Null"));
        assert!(retained.is_indirect());
        assert!(retained.is_null());
        assert!(retained.is_same_object_as(&indirect_null));
    }

    #[test]
    fn replace_key_preserves_a_dangling_indirect_reference_and_its_identity() {
        let dangling = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        dangling.set_resolved(ObjectValue::Null);
        let dict = ObjectHandle::dictionary(vec![]);

        dict.replace_key(b"/Dangling", dangling.clone()).unwrap();

        let retained = dict.get_key(b"/Dangling");
        assert!(!dict.has_key(b"/Dangling"));
        assert!(retained.is_indirect());
        assert!(retained.is_null());
        assert!(retained.is_same_object_as(&dangling));
    }

    #[test]
    fn replace_key_on_a_non_dictionary_handle_reports_qpdf_type_warning() {
        let scalar = ObjectHandle::integer(1);
        let error = scalar
            .replace_key(b"/A", ObjectHandle::integer(2))
            .expect_err("a contextless qpdf type warning is an exception");

        assert!(matches!(
            error,
            Error::System(message)
                if message == "operation for dictionary attempted on object of type integer: ignoring key replacement request"
        ));
        assert_eq!(scalar.as_integer(), Some(1));
    }

    #[test]
    fn replace_array_item_preserves_identity_and_rejects_invalid_slots() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        let replacement = ObjectHandle::dictionary(vec![]);
        let retained = replacement.clone();

        assert!(array.replace_array_item(0, replacement));
        retained
            .replace_key(b"/K", ObjectHandle::integer(9))
            .unwrap();
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
        pdf.resolve(&pages).expect("resolve loaded Pages object");
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
    fn replace_key_rejects_foreign_ownership_without_mutating_the_dictionary() {
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let destination =
            ObjectHandle::dictionary(vec![(b"/Existing".to_vec(), ObjectHandle::integer(1))]);
        destination.promote_to_indirect(ObjectRef::new(40, 0), 4242, Rc::downgrade(&resolver));
        let foreign = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(41, 0),
            NO_PARSED_OFFSET,
            4243,
            Rc::downgrade(&resolver),
        );

        let error = destination
            .replace_key(b"/Foreign", foreign)
            .expect_err("replaceKey must reject a value owned by another QPDF");

        assert!(matches!(
            error,
            Error::Internal(message) if message == FOREIGN_OBJECT_OWNERSHIP_ERROR
        ));
        assert!(!destination.has_key(b"/Foreign"));
        assert_eq!(destination.get_key(b"/Existing").as_integer(), Some(1));
    }

    #[test]
    fn replace_key_accepts_a_foreign_descendant_nested_in_a_direct_container() {
        // `QPDFObjectHandle::checkOwnership` (`libqpdf/QPDFObjectHandle.cc:
        // 2355-2365`) compares only `this->getOwningQPDF()` and
        // `item.getOwningQPDF()` -- both O(1) reads of the *top-level*
        // handle's own owning-document pointer -- and never walks `item`'s
        // descendants. `QPDF_Array::checkOwnership` (`QPDF_Array.cc:10-26`)
        // is the same shape. A programmatically constructed direct value's
        // own `getOwningQPDF()` is `nullptr` regardless of what it contains
        // (file-parser-created direct values are the exception: qpdf stamps
        // them through `QPDFParser::setDescription`, `QPDFParser.cc:439-443`),
        // so real qpdf's `replaceKey` accepts a direct
        // container that nests a foreign indirect object several levels
        // down -- silently embedding an out-of-context object reference,
        // per qpdf's own `copyForeignObject` guidance for the caller to
        // avoid this. This is a known qpdf footgun, not a guard qpdf
        // implements; flpdf's `replace_key` must match qpdf's actual
        // (shallow) `checkOwnership`, not invent a deeper one.
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let destination = ObjectHandle::dictionary(vec![]);
        destination.promote_to_indirect(ObjectRef::new(42, 0), 4242, Rc::downgrade(&resolver));
        let foreign = ObjectHandle::new_indirect_for_pdf_with_resolver(
            ObjectRef::new(43, 0),
            NO_PARSED_OFFSET,
            4243,
            Rc::downgrade(&resolver),
        );
        let direct_container = ObjectHandle::dictionary(vec![(b"/Foreign".to_vec(), foreign)]);

        destination
            .replace_key(b"/Container", direct_container)
            .expect("checkOwnership does not inspect a direct value's descendants");

        assert!(destination.has_key(b"/Container"));
        assert!(destination
            .get_key(b"/Container")
            .get_key(b"/Foreign")
            .is_indirect());
    }

    #[test]
    fn replace_key_removes_the_key_for_a_direct_null_previously_contained_by_another_document() {
        // Regression test for a chatgpt-codex-connector finding on PR #791
        // (databaseId 3773208253): a direct null handle that was earlier a
        // descendant of a PDF-A indirect object picks up PDF A's id in its
        // `pdf_unique_ids` live-containment bookkeeping (`promote_to_indirect`
        // -> `associate_pdf_identity`). That bookkeeping tracks *current*
        // containment for dirty-marking, not qpdf's notion of ownership
        // (`getOwningQPDF()`, set only by `setObjGen`/indirect promotion --
        // see `replace_key_accepts_a_foreign_descendant_nested_in_a_direct_
        // container` above), and never clears when the null value is no
        // longer reachable from PDF A. Using it to drive the ownership check
        // wrongly rejects this direct null when it is later passed to
        // `replace_key` on a PDF-B dictionary, even though this programmatic
        // direct value is unowned in qpdf and `QPDF_Dictionary::replaceKey`'s
        // null-removes-key branch (`QPDF_Dictionary.cc:135-146`) would run
        // unconditionally after qpdf's own (shallow, `object_ref`-only)
        // `checkOwnership` passes.
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let stale_null = ObjectHandle::null();
        let pdf_a_container = ObjectHandle::dictionary(vec![(b"/X".to_vec(), stale_null.clone())]);
        pdf_a_container.promote_to_indirect(ObjectRef::new(60, 0), 4242, Rc::downgrade(&resolver));

        let destination =
            ObjectHandle::dictionary(vec![(b"/K".to_vec(), ObjectHandle::integer(1))]);
        destination.promote_to_indirect(ObjectRef::new(61, 0), 4243, Rc::downgrade(&resolver));

        destination.replace_key(b"/K", stale_null).expect(
            "a programmatic direct null is unowned in qpdf, regardless of prior containment",
        );

        assert!(!destination.has_key(b"/K"));
    }

    #[test]
    fn replace_key_accepts_a_direct_scalar_previously_contained_by_another_document() {
        // Companion to the null regression above: the fix must not be a
        // null-specific exemption. qpdf's `checkOwnership` never associates
        // ownership with a direct value through containment, for any type,
        // so a direct (non-null) scalar with the same stale `pdf_unique_ids`
        // history must be accepted and inserted, not rejected.
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let stale_integer = ObjectHandle::integer(7);
        let pdf_a_container =
            ObjectHandle::dictionary(vec![(b"/Y".to_vec(), stale_integer.clone())]);
        pdf_a_container.promote_to_indirect(ObjectRef::new(62, 0), 4242, Rc::downgrade(&resolver));

        let destination = ObjectHandle::dictionary(vec![]);
        destination.promote_to_indirect(ObjectRef::new(63, 0), 4243, Rc::downgrade(&resolver));

        destination.replace_key(b"/Int", stale_integer).expect(
            "a programmatic direct scalar is unowned in qpdf, regardless of prior containment",
        );

        assert_eq!(destination.get_key(b"/Int").as_integer(), Some(7));
    }

    #[test]
    fn set_array_item_accepts_a_direct_null_previously_contained_by_another_document() {
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let stale_null = ObjectHandle::null();
        let pdf_a_container = ObjectHandle::dictionary(vec![(b"/X".to_vec(), stale_null.clone())]);
        pdf_a_container.promote_to_indirect(ObjectRef::new(64, 0), 4242, Rc::downgrade(&resolver));

        let destination = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        destination.promote_to_indirect(ObjectRef::new(65, 0), 4243, Rc::downgrade(&resolver));

        destination.set_array_item(0, stale_null).expect(
            "a programmatic direct null is unowned in qpdf, regardless of prior containment",
        );

        assert!(destination.try_array_item(0).unwrap().unwrap().is_null());
    }

    #[test]
    fn append_array_item_accepts_a_direct_scalar_previously_contained_by_another_document() {
        let (_, resolver) = super::identity_tests::resolver_bearing_handle(ObjectValue::Null);
        let stale_integer = ObjectHandle::integer(7);
        let pdf_a_container =
            ObjectHandle::dictionary(vec![(b"/Y".to_vec(), stale_integer.clone())]);
        pdf_a_container.promote_to_indirect(ObjectRef::new(66, 0), 4242, Rc::downgrade(&resolver));

        let destination = ObjectHandle::array(vec![]);
        destination.promote_to_indirect(ObjectRef::new(67, 0), 4243, Rc::downgrade(&resolver));

        destination.append_array_item(stale_integer).expect(
            "a programmatic direct scalar is unowned in qpdf, regardless of prior containment",
        );

        assert_eq!(
            destination.try_array_item(0).unwrap().unwrap().as_integer(),
            Some(7)
        );
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
    fn array_mutators_reject_uninitialized_items() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(0)]);
        let uninitialized = ObjectHandle::uninitialized();

        let append_error = array
            .append_array_item(uninitialized.clone())
            .expect_err("an uninitialized handle must not be appended");
        assert!(matches!(
            append_error,
            Error::Internal(message)
                if message == "Attempting to add an uninitialized object to a QPDF_Array."
        ));

        let insert_error = array
            .insert_array_item(0, uninitialized.clone())
            .expect_err("an uninitialized handle must not be inserted");
        assert!(matches!(
            insert_error,
            Error::Internal(message)
                if message == "Attempting to add an uninitialized object to a QPDF_Array."
        ));

        let set_error = array
            .set_array_item(0, uninitialized)
            .expect_err("an uninitialized handle must not be set into an array slot");
        assert!(matches!(
            set_error,
            Error::Internal(message)
                if message == "Attempting to add an uninitialized object to a QPDF_Array."
        ));

        assert_eq!(array.try_array_len().unwrap(), Some(1));
        assert_eq!(
            array.try_array_item(0).unwrap().unwrap().as_integer(),
            Some(0)
        );
    }

    #[test]
    fn replace_key_rejects_inserting_a_direct_dictionary_into_itself() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]);
        let self_clone = dict.clone();
        dict.replace_key(b"/Self", self_clone).unwrap();
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

        target.replace_key(b"/Self", replacement.clone()).unwrap();

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
        indirect.replace_key(b"/Self", indirect.clone()).unwrap();
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
        let ObjectValue::Dictionary(entries) = &*state else {
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

        owner
            .replace_key(b"/Nested", ObjectHandle::dictionary(vec![]))
            .unwrap();

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
            filter_on_write: true,
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
            filter_on_write: true,
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

        owner.set_resolved(ObjectValue::Null);
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
        first.replace_key(b"/Second", second.clone()).unwrap();
        second.replace_key(b"/First", first.clone()).unwrap();
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
        copy.replace_key(b"/A", ObjectHandle::integer(2)).unwrap();
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
            .replace_key(b"/Inner", ObjectHandle::integer(2))
            .unwrap();
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
    fn has_key_omits_a_present_null_value_like_qpdf() {
        let dict = ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::null())]);
        assert!(!dict.has_key(b"/A"));
        assert!(!dict.has_key(b"/Missing"));
    }

    #[test]
    fn get_key_missing_key_carries_parent_context_like_qpdf() {
        let dict = ObjectHandle::dictionary(vec![]);
        let missing = dict.get_key(b"/Missing");
        let error = missing
            .try_get_keys()
            .expect_err("missing-key null should retain its dictionary context");
        assert!(matches!(
            error,
            crate::Error::System(ref message)
                if message
                    == " -> dictionary key /Missing: operation for dictionary attempted on object of type null: treating as empty"
        ));
    }

    #[test]
    fn try_has_key_on_a_non_dictionary_handle_reports_qpdf_type_warning() {
        let scalar = ObjectHandle::integer(1);
        assert!(matches!(
            scalar.try_has_key(b"/A"),
            Err(crate::Error::System(message))
                if message == "operation for dictionary attempted on object of type integer: returning false for a key containment request"
        ));
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

    #[test]
    fn make_resources_indirect_promotes_direct_second_level_values_only() {
        let mut pdf = crate::Pdf::empty().expect("empty PDF");
        let direct_font = ObjectHandle::integer(1);
        let direct_font_alias = direct_font.clone();
        let direct_nested =
            ObjectHandle::dictionary(vec![(b"/Child".to_vec(), ObjectHandle::integer(2))]);
        let direct_nested_alias = direct_nested.clone();
        let already_indirect = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .expect("allocate an already-indirect resource");
        let direct_category = ObjectHandle::dictionary(vec![
            (b"/F1".to_vec(), direct_font),
            (b"/F2".to_vec(), already_indirect.clone()),
            (b"/Nested".to_vec(), direct_nested),
        ]);
        let resources = ObjectHandle::dictionary(vec![
            (b"/Font".to_vec(), direct_category),
            (
                b"/ProcSet".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::name(b"PDF".to_vec())]),
            ),
        ]);

        resources
            .make_resources_indirect(&mut pdf)
            .expect("promote direct resource values");

        let font = resources.get_key(b"/Font");
        let promoted_font = font.get_key(b"/F1");
        assert!(promoted_font.is_indirect());
        assert!(direct_font_alias.is_indirect());
        assert!(promoted_font.is_same_object_as(&direct_font_alias));
        assert_eq!(promoted_font.as_integer(), Some(1));
        assert!(font.get_key(b"/F2").is_same_object_as(&already_indirect));

        let promoted_nested = font.get_key(b"/Nested");
        assert!(promoted_nested.is_indirect());
        assert!(direct_nested_alias.is_indirect());
        assert!(promoted_nested.is_same_object_as(&direct_nested_alias));
        assert!(promoted_nested.get_key(b"/Child").is_direct());

        assert!(
            font.is_direct(),
            "the category dictionary itself stays direct"
        );
        assert!(resources.get_key(b"/ProcSet").is_direct());
    }

    #[test]
    fn make_resources_indirect_is_a_noop_for_non_dictionary_receivers() {
        let mut pdf = crate::Pdf::empty().expect("empty PDF");
        let value = ObjectHandle::integer(1);

        value
            .make_resources_indirect(&mut pdf)
            .expect("non-dictionary receivers are ignored");

        assert!(value.is_direct());
        assert_eq!(value.as_integer(), Some(1));
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
    fn merge_resources_resolves_indirect_scalar_array_items_before_union() {
        // qpdf's `isScalar()` dereferences every array item before the
        // unparsed-text union (`QPDFObjectHandle.cc:1130-1146,449-452`).
        let (destination_item, destination_resolver) =
            crate::object_handle::identity_tests::resolver_bearing_handle(ObjectValue::Name(
                b"PDF".to_vec(),
            ));
        let dest = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![destination_item.clone()]),
        )]);
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"Text".to_vec())]),
        )]);

        dest.merge_resources(&other, None)
            .expect("indirect scalar array items merge");

        let items = dest
            .get_key(b"/ProcSet")
            .as_array()
            .expect("merged ProcSet remains an array");
        assert_eq!(items.len(), 2);
        assert!(items[0].ptr_eq(&destination_item));
        assert_eq!(items[0].as_name(), Some(b"PDF".to_vec()));
        assert_eq!(items[1].as_name(), Some(b"Text".to_vec()));
        drop(destination_resolver);
    }

    #[test]
    fn merge_resources_propagates_indirect_array_item_resolution_errors() {
        // A qpdf accessor failure escapes mergeResources; it is not converted
        // into a non-scalar skip (`QPDFObjectHandle.cc:1130-1146`).
        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(91, 0), -1);
        let dest =
            ObjectHandle::dictionary(vec![(b"ProcSet".to_vec(), ObjectHandle::array(vec![]))]);
        let other = ObjectHandle::dictionary(vec![(
            b"ProcSet".to_vec(),
            ObjectHandle::array(vec![unresolved]),
        )]);

        let error = dest
            .merge_resources(&other, None)
            .expect_err("array item resolution failure must propagate");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "object 91 0 belongs to a dropped PDF"
        ));
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
            filter_on_write: true,
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
            filter_on_write: true,
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
            filter_on_write: true,
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
            filter_on_write: true,
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
    fn replace_stream_data_sets_filter_and_decode_parms_when_given() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            stream_dict: dict.clone(),
            stream_data: Some(Rc::new(b"old".to_vec())),
            stream_provider: None,
            filter_on_write: true,
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
            filter_on_write: true,
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
            filter_on_write: true,
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
            filter_on_write: true,
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
                filter_on_write: true,
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
        indirect
            .replace_key(b"/A", ObjectHandle::integer(1))
            .unwrap();
        assert_eq!(indirect.get_key(b"/A").as_integer(), Some(1));
        indirect.remove_key(b"/A");
        assert!(indirect.try_get_key(b"/A").unwrap().is_null());
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
        // Canonical replacement. New direct descendants must inherit the same
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

        direct.replace_key(b"/Indirect", indirect.clone()).unwrap();

        assert!(direct.get_key(b"/Indirect").is_same_object_as(&indirect));
        assert!(indirect.containing_object_refs().is_empty());
    }

    #[test]
    fn replace_key_on_an_unresolved_indirect_handle_propagates_resolution_error() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        let error = indirect
            .replace_key(b"/A", ObjectHandle::integer(1))
            .expect_err("qpdf resolves the receiver before replacing a key");

        assert!(matches!(
            error,
            Error::Internal(message) if message == "object 1 0 belongs to a dropped PDF"
        ));
        indirect.remove_key(b"/A"); // legacy no-resolution helper remains safe
        assert!(indirect.try_get_key(b"/A").is_err());
    }

    #[test]
    fn shallow_copy_of_an_unresolved_indirect_handle_reports_qpdf_error() {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1);
        let error = indirect
            .shallow_copy()
            .expect_err("QPDF_Unresolved::copy is a logic error");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "attempted to shallow copy an unresolved QPDFObjectHandle"
        ));
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
            filter_on_write: true,
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
        assert!(is_scalar(&ObjectHandle::boolean(true)).unwrap());
        assert!(is_scalar(&ObjectHandle::integer(1)).unwrap());
        assert!(is_scalar(&ObjectHandle::name(b"N".to_vec())).unwrap());
        assert!(is_scalar(&ObjectHandle::null()).unwrap());
        assert!(is_scalar(&ObjectHandle::real(1.0)).unwrap());
        assert!(is_scalar(&ObjectHandle::string(b"S".to_vec())).unwrap());
        assert!(!is_scalar(&ObjectHandle::array(vec![])).unwrap());
    }

    #[test]
    fn merge_resources_mints_a_second_unique_name_when_the_first_candidate_is_taken() {
        // this_val (the Font sub-dict itself) has a nested dictionary-valued
        // entry ("Widths") whose own key happens to be "F1_1" --
        // try_get_resource_names is called ON this_val (see merge_resources's
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

    #[test]
    fn merge_resources_resolves_nested_dictionary_values_for_unique_names() {
        // qpdf's getResourceNames() calls isDictionary() on every value,
        // which dereferences an indirect nested dictionary before collecting
        // its keys (`QPDFObjectHandle.cc:1156-1170,431-434`).
        let (indirect_widths, resolver) =
            crate::object_handle::identity_tests::resolver_bearing_handle(ObjectValue::Dictionary(
                [(b"F1_1".to_vec(), ObjectHandle::integer(0))]
                    .into_iter()
                    .collect(),
            ));
        let this_font = ObjectHandle::dictionary(vec![
            (b"F1".to_vec(), ObjectHandle::integer(1)),
            (b"Widths".to_vec(), indirect_widths.clone()),
        ]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();

        dest.merge_resources(&other, Some(&mut conflicts))
            .expect("nested dictionary value resolves");

        let new_name = conflicts
            .get(b"/Font".as_slice())
            .and_then(|m| m.get(b"/F1".as_slice()))
            .expect("F1 conflict recorded");
        assert_eq!(new_name, b"/F1_2");
        assert!(dest
            .get_key(b"/Font")
            .get_key(b"/Widths")
            .ptr_eq(&indirect_widths));
        drop(resolver);
    }

    #[test]
    fn merge_resources_propagates_nested_dictionary_resolution_errors() {
        let unresolved = ObjectHandle::new_indirect_unresolved(ObjectRef::new(92, 0), -1);
        let this_font = ObjectHandle::dictionary(vec![
            (b"F1".to_vec(), ObjectHandle::integer(1)),
            (b"Widths".to_vec(), unresolved),
        ]);
        let dest = ObjectHandle::dictionary(vec![(b"Font".to_vec(), this_font)]);
        let other_font = ObjectHandle::dictionary(vec![(b"F1".to_vec(), ObjectHandle::integer(2))]);
        let other = ObjectHandle::dictionary(vec![(b"Font".to_vec(), other_font)]);
        let mut conflicts = std::collections::BTreeMap::new();

        let error = dest
            .merge_resources(&other, Some(&mut conflicts))
            .expect_err("nested dictionary resolution failure must propagate");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "object 92 0 belongs to a dropped PDF"
        ));
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

    struct RetryOnlyProvider;

    impl StreamDataProvider for RetryOnlyProvider {
        fn provide_stream_data_with_retry_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            _pipeline: &mut dyn Pipeline,
            _suppress_warnings: bool,
            _will_retry: bool,
        ) -> Result<bool> {
            Ok(true)
        }
    }

    struct LegacyOnlyProviderWithRetryFlag;

    impl StreamDataProvider for LegacyOnlyProviderWithRetryFlag {
        fn supports_retry(&self) -> bool {
            true
        }

        fn provide_stream_data_by_id(
            &self,
            _object_number: u32,
            _generation: u16,
            _pipeline: &mut dyn Pipeline,
        ) -> Result<()> {
            Ok(())
        }
    }

    struct FailingCallbackPipeline {
        failure: &'static str,
    }

    impl Pipeline for FailingCallbackPipeline {
        fn identifier(&self) -> &str {
            "failing callback pipeline"
        }

        fn write(&mut self, _data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            Err(PipelineError::runtime(self.failure))
        }

        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            Err(PipelineError::runtime(self.failure))
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
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
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
            .replace_key(b"/Length", ObjectHandle::integer(99))
            .unwrap();

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
        let error = stream
            .replace_stream_data_provider(
                Rc::new(PipeProvider::new(Rc::new(b"direct bytes".to_vec()))),
                None,
                None,
            )
            .expect_err("provider registration requires an indirect stream identity");
        assert!(matches!(
            error,
            Error::System(message)
                if message == STREAM_DATA_PROVIDER_REQUIRES_INDIRECT_ERROR
        ));
    }

    #[test]
    fn supports_retry_does_not_fallback_between_callback_families() {
        let pdf = crate::Pdf::empty().expect("empty PDF");

        let mut retry_only_pipeline = crate::pipeline::buffer::Buffer::new("retry-only", None);
        RetryOnlyProvider
            .provide_stream_data_with_retry_by_id(47, 0, &mut retry_only_pipeline, false, false)
            .expect("retry-only override");

        let normal_family_stream = pdf.new_stream().expect("normal-family stream");
        normal_family_stream
            .replace_stream_data_provider(Rc::new(RetryOnlyProvider), None, None)
            .expect("normal-family provider replacement");
        let error = normal_family_stream
            .get_raw_stream_data()
            .expect_err("normal family must not call retry-only override");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == STREAM_DATA_PROVIDER_DEFAULT_ERROR
        ));

        let mut legacy_only_pipeline = crate::pipeline::buffer::Buffer::new("legacy-only", None);
        LegacyOnlyProviderWithRetryFlag
            .provide_stream_data_by_id(47, 0, &mut legacy_only_pipeline)
            .expect("legacy-only override");

        let retry_family_stream = pdf.new_stream().expect("retry-family stream");
        retry_family_stream
            .replace_stream_data_provider(Rc::new(LegacyOnlyProviderWithRetryFlag), None, None)
            .expect("retry-family provider replacement");
        let error = retry_family_stream
            .get_raw_stream_data()
            .expect_err("retry family must not call normal-only override");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == STREAM_DATA_PROVIDER_DEFAULT_ERROR
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
    fn callback_adapters_propagate_errors_and_preserve_repeated_invocation() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let calls = Rc::new(Cell::new(0));
        let callback_calls = Rc::clone(&calls);
        stream
            .replace_stream_data_with_callback(
                move |_pipeline| {
                    callback_calls.set(callback_calls.get() + 1);
                    Err(Error::System("callback failure".to_owned()))
                },
                None,
                None,
            )
            .expect("callback provider replacement");

        for _ in 0..2 {
            let error = stream
                .get_raw_stream_data()
                .expect_err("callback error must cross the provider boundary");
            assert!(matches!(
                error,
                Error::System(message) if message == "callback failure"
            ));
        }
        assert_eq!(
            calls.get(),
            2,
            "callback must remain reusable after an error"
        );
    }

    #[test]
    fn callback_adapters_propagate_pipeline_write_and_finish_errors() {
        let mut write_pipeline = FailingCallbackPipeline {
            failure: "callback write failure",
        };
        assert_eq!(write_pipeline.identifier(), "failing callback pipeline");
        let write_provider = CallbackProvider {
            callback: |pipeline: &mut dyn Pipeline| {
                pipeline.write(b"write failure").map_err(Error::from)
            },
        };
        assert!(matches!(
            write_provider
                .provide_stream_data(ObjectRef::new(47, 0), &mut write_pipeline)
                .expect_err("callback write failure must propagate"),
            Error::System(message) if message == "callback write failure"
        ));

        let mut finish_pipeline = FailingCallbackPipeline {
            failure: "callback finish failure",
        };
        let finish_provider = CallbackProvider {
            callback: |pipeline: &mut dyn Pipeline| pipeline.finish().map_err(Error::from),
        };
        assert!(matches!(
            finish_provider
                .provide_stream_data(ObjectRef::new(47, 0), &mut finish_pipeline)
                .expect_err("callback finish failure must propagate"),
            Error::System(message) if message == "callback finish failure"
        ));
    }

    #[test]
    fn retry_callback_adapter_forwards_both_flag_combinations() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
        let flags = Rc::new(RefCell::new(Vec::new()));
        let callback_flags = Rc::clone(&flags);
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    callback_flags
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    pipeline
                        .write(b"retry callback bytes")
                        .map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)?;
                    Ok(true)
                },
                None,
                None,
            )
            .expect("retry callback provider replacement");

        assert_eq!(
            stream
                .get_raw_stream_data()
                .expect("default retry callback pipe")
                .as_slice(),
            b"retry callback bytes"
        );
        let mut sink = crate::pipeline::buffer::Buffer::new("retry callback", None);
        let mut filtering_attempted = false;
        assert!(stream
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                crate::writer::DecodeLevel::None,
                true,
                true,
            )
            .expect("explicit retry callback pipe"));
        assert_eq!(
            sink.take_buffer().expect("retry callback output"),
            b"retry callback bytes"
        );
        assert_eq!(*flags.borrow(), vec![(false, false), (true, true)]);
    }

    #[test]
    fn replacing_a_provider_with_a_buffer_releases_the_provider_source() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
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
                filter_on_write: true,
                ..
            })
        )));
    }

    #[test]
    fn provider_replacement_uses_qpdf_filter_boundary_for_uninitialized_and_null_values() {
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf
            .new_stream_with_data(Rc::new(b"old".to_vec()))
            .expect("owned stream");
        let dict = stream.as_stream_dict().expect("stream dictionary");
        dict.replace_key(b"/Filter", ObjectHandle::name(b"Keep".to_vec()))
            .expect("filter key");
        dict.replace_key(b"/DecodeParms", ObjectHandle::dictionary(vec![]))
            .expect("decode parms key");

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
        let pdf = crate::Pdf::empty().expect("empty PDF");
        let stream = pdf.new_stream().expect("owned empty stream");
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
    use std::collections::BTreeMap;

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
    fn try_has_key_on_a_non_dictionary_emits_qpdf_warning_and_returns_false() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        assert!(!handle.try_has_key(b"/A").unwrap());

        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for dictionary attempted on object of type integer: returning false for a key containment request"]
        );
    }

    #[test]
    fn try_get_key_on_a_non_dictionary_uses_qpdf_null_context_description() {
        let (handle, recorder) = handle_resolving(ObjectValue::Integer(7));

        let null = handle.try_get_key(b"/A").unwrap();

        assert!(null.is_null());
        assert_eq!(
            null.description(),
            "object 3 0 -> null returned from getting key  from non-Dictionary"
        );
        assert_eq!(
            warnings(&recorder),
            ["object 3 0: operation for dictionary attempted on object of type integer: returning null for attempted key retrieval"]
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
    fn qpdf_type_check_accessors_return_fallbacks_and_warn() {
        let (integer, recorder) = handle_resolving(ObjectValue::Integer(7));
        let (dictionary, dictionary_recorder) =
            handle_resolving(ObjectValue::Dictionary(BTreeMap::new()));

        assert!(!integer.try_get_bool_value().unwrap());
        assert_eq!(integer.try_get_real_value().unwrap(), b"0.0");
        assert_eq!(integer.try_get_name().unwrap(), b"/QPDFFakeName");
        assert!(integer.try_get_string_value().unwrap().is_empty());
        assert!(integer.try_get_utf8_value().unwrap().is_empty());
        assert_eq!(integer.try_get_operator_value().unwrap(), b"QPDFFAKE");
        assert!(integer.try_get_inline_image_value().unwrap().is_empty());
        assert_eq!(dictionary.try_get_int_value().unwrap(), 0);
        assert_eq!(dictionary.try_get_numeric_value().unwrap(), 0.0);

        assert_eq!(warnings(&recorder).len(), 7);
        assert_eq!(warnings(&dictionary_recorder).len(), 2);
        assert!(warnings(&recorder)
            .iter()
            .all(|message| message.contains("object 3 0: operation for")));
        assert!(warnings(&dictionary_recorder)
            .iter()
            .all(|message| message.contains("object 3 0: operation for")));
    }

    #[test]
    fn qpdf_type_check_container_accessors_keep_qpdf_fallbacks() {
        let (integer, recorder) = handle_resolving(ObjectValue::Integer(7));
        let null = ObjectHandle::null();

        assert_eq!(integer.try_get_array_n_items().unwrap(), 0);
        assert!(integer.try_get_array_as_vector().unwrap().is_empty());
        assert!(integer.try_get_array_item(-1).unwrap().is_null());
        assert!(!integer.try_get_has_key(b"/Potato").unwrap());
        assert!(integer.try_get_dict_as_map().unwrap().is_empty());
        assert!(integer.try_get_key_if_dict(b"/Potato").unwrap().is_null());
        assert!(null.try_get_key_if_dict(b"/Integer").unwrap().is_null());

        assert_eq!(warnings(&recorder).len(), 6);
        assert!(warnings(&recorder)
            .iter()
            .any(|message| message.contains("returning null")));
    }

    #[test]
    fn qpdf_uninitialized_handles_are_distinct_from_null_and_unresolved_values() {
        let handle = ObjectHandle::default();

        assert!(!handle.is_initialized());
        assert!(!handle.is_null());
        assert!(!handle.is_resolved());
        assert!(!handle.try_is_integer().unwrap());
        assert!(!handle.try_is_array().unwrap());
        assert!(!handle.try_is_dictionary().unwrap());
        assert!(!handle.try_is_name().unwrap());
        assert!(!handle.try_is_scalar().unwrap());
        assert!(!handle.try_is_number().unwrap());
        assert_eq!(handle.type_code().unwrap(), 0);
        assert_eq!(handle.type_name().unwrap(), "uninitialized");
        assert!(!handle.try_is_rectangle().unwrap());
        assert_eq!(
            handle.try_get_array_as_rectangle().unwrap(),
            Rectangle::default()
        );
        assert!(!handle.try_is_matrix().unwrap());
        assert_eq!(
            handle.try_get_array_as_matrix().unwrap(),
            ObjectHandleMatrix::default()
        );
        assert!(matches!(
            handle.try_dereference(),
            Err(crate::Error::Internal(message))
                if message == "attempted to dereference an uninitialized QPDFObjectHandle"
        ));

        let mut bytes = Vec::new();
        let mut output = crate::pipeline::PlString::new("uninitialized", None, &mut bytes);
        let error = handle
            .write_json(2, &mut output, true, 0)
            .expect_err("JSON dereference must reject an uninitialized handle");
        assert_eq!(
            error.to_string(),
            "attempted to dereference an uninitialized QPDFObjectHandle"
        );
    }

    #[test]
    fn qpdf_geometry_helpers_use_zero_fallbacks_and_silent_number_checks() {
        let rectangle = ObjectHandle::new_from_rectangle(Rectangle::new(7.8, 5.6, 1.2, 3.4));
        assert!(rectangle.try_is_rectangle().unwrap());
        assert_eq!(
            rectangle.try_get_array_as_rectangle().unwrap(),
            Rectangle::new(1.2, 3.4, 7.8, 5.6)
        );

        let invalid_rectangle = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::integer(2),
            ObjectHandle::integer(3),
            ObjectHandle::boolean(false),
        ]);
        assert!(!invalid_rectangle.try_is_rectangle().unwrap());
        assert_eq!(
            invalid_rectangle.try_get_array_as_rectangle().unwrap(),
            Rectangle::default()
        );
        let integer_rectangle = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::integer(2),
            ObjectHandle::integer(3),
            ObjectHandle::integer(4),
        ]);
        assert!(integer_rectangle.try_is_rectangle().unwrap());
        assert_eq!(
            integer_rectangle.try_get_array_as_rectangle().unwrap(),
            Rectangle::new(1.0, 2.0, 3.0, 4.0)
        );
        let literal_rectangle = ObjectHandle::array(vec![
            ObjectHandle::real_literal(1.2, b"1.2".to_vec()),
            ObjectHandle::real_literal(3.4, b"3.4".to_vec()),
            ObjectHandle::real_literal(5.6, b"5.6".to_vec()),
            ObjectHandle::real_literal(7.8, b"7.8".to_vec()),
        ]);
        assert_eq!(
            literal_rectangle.try_get_array_as_rectangle().unwrap(),
            Rectangle::new(1.2, 3.4, 5.6, 7.8)
        );
        assert!(!ObjectHandle::integer(1).try_is_rectangle().unwrap());
        let uninitialized_rectangle = ObjectHandle::array(vec![
            ObjectHandle::uninitialized(),
            ObjectHandle::integer(2),
            ObjectHandle::integer(3),
            ObjectHandle::integer(4),
        ]);
        assert_eq!(
            uninitialized_rectangle
                .try_get_array_as_rectangle()
                .unwrap(),
            Rectangle::default()
        );

        let matrix =
            ObjectHandle::new_from_matrix(ObjectHandleMatrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3));
        assert!(matrix.try_is_matrix().unwrap());
        assert_eq!(
            matrix.try_get_array_as_matrix().unwrap(),
            ObjectHandleMatrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3)
        );
        let qpdf_matrix =
            ObjectHandle::new_from_qpdf_matrix(crate::Matrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3));
        assert_eq!(
            qpdf_matrix.try_get_array_as_matrix().unwrap(),
            ObjectHandleMatrix::new(1.2, 3.4, 5.6, 7.8, 9.1, 2.3)
        );

        let invalid_matrix = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::integer(2),
            ObjectHandle::integer(3),
            ObjectHandle::boolean(false),
            ObjectHandle::integer(5),
            ObjectHandle::integer(6),
        ]);
        assert!(!invalid_matrix.try_is_matrix().unwrap());
        assert_eq!(
            invalid_matrix.try_get_array_as_matrix().unwrap(),
            ObjectHandleMatrix::default()
        );
    }

    #[test]
    fn qpdf_cursors_return_live_children_and_uninitialized_end_values() {
        let array = ObjectHandle::array(vec![
            ObjectHandle::name(b"Item0".to_vec()),
            ObjectHandle::name(b"Item1".to_vec()),
            ObjectHandle::name(b"Item2".to_vec()),
        ]);
        let items = array.try_array_items().unwrap();
        let mut cursor = items.begin();
        let held = cursor.current();
        assert_eq!(held.try_get_name().unwrap(), b"/Item0");
        cursor.previous();
        assert_eq!(held.try_get_name().unwrap(), b"/Item0");
        cursor.next();
        cursor.next();
        cursor.next();
        assert!(cursor.is_end());
        assert!(!held.is_initialized());
        cursor.previous();
        assert_eq!(held.try_get_name().unwrap(), b"/Item2");

        let dictionary = ObjectHandle::dictionary(vec![
            (b"Key1".to_vec(), ObjectHandle::name(b"Value1".to_vec())),
            (b"Key2".to_vec(), ObjectHandle::name(b"Value2".to_vec())),
        ]);
        let items = dictionary.try_dict_items().unwrap();
        let mut cursor = items.begin();
        let entry = cursor.current();
        assert_eq!(entry.key, b"/Key1");
        assert_eq!(entry.value.try_get_name().unwrap(), b"/Value1");
        cursor.next();
        cursor.next();
        assert!(cursor.is_end());
        assert!(!entry.value.is_initialized());
        cursor.previous();
        assert_eq!(cursor.current().key, b"/Key2");

        let empty_array_items = ObjectHandle::integer(7).try_array_items().unwrap();
        assert!(empty_array_items.end().is_end());
        let empty_dict_items = ObjectHandle::integer(7).try_dict_items().unwrap();
        assert!(empty_dict_items.end().is_end());
    }

    #[test]
    fn qpdf_type_check_accessors_cover_successful_values_and_live_containers() {
        assert!(ObjectHandle::boolean(true).try_get_bool_value().unwrap());
        assert!(ObjectHandle::integer(7).try_is_scalar().unwrap());
        assert_eq!(
            ObjectHandle::real(1.25).try_get_real_value().unwrap(),
            b"1.25"
        );
        assert_eq!(
            ObjectHandle::real_literal(0.4, b".4".to_vec())
                .try_get_real_value()
                .unwrap(),
            b".4"
        );
        assert_eq!(
            ObjectHandle::integer(7).try_get_numeric_value().unwrap(),
            7.0
        );
        assert_eq!(
            ObjectHandle::real(1.25).try_get_numeric_value().unwrap(),
            1.25
        );
        assert_eq!(
            ObjectHandle::real_literal(0.4, b".4".to_vec())
                .try_get_numeric_value()
                .unwrap(),
            0.4
        );
        assert_eq!(
            ObjectHandle::name(b"Name".to_vec()).try_get_name().unwrap(),
            b"/Name"
        );
        assert_eq!(
            ObjectHandle::string(vec![0xfe, 0xff, 0x00, b'A'])
                .try_get_string_value()
                .unwrap(),
            vec![0xfe, 0xff, 0x00, b'A']
        );
        assert_eq!(
            ObjectHandle::string(vec![0xfe, 0xff, 0x00, b'A'])
                .try_get_utf8_value()
                .unwrap(),
            b"A"
        );
        assert_eq!(
            ObjectHandle::operator(b"q".to_vec())
                .try_get_operator_value()
                .unwrap(),
            b"q"
        );
        assert_eq!(
            ObjectHandle::inline_image(b"raw".to_vec())
                .try_get_inline_image_value()
                .unwrap(),
            b"raw"
        );

        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        assert!(array.try_is_array().unwrap());
        assert!(!array.try_is_integer().unwrap());
        assert_eq!(array.try_get_array_n_items().unwrap(), 1);
        assert_eq!(array.try_get_array_as_vector().unwrap().len(), 1);
        assert_eq!(array.try_get_array_item(0).unwrap().as_integer(), Some(1));

        let dictionary = ObjectHandle::dictionary(vec![
            (b"A".to_vec(), ObjectHandle::integer(1)),
            (b"B".to_vec(), ObjectHandle::null()),
        ]);
        assert!(dictionary.try_is_dictionary().unwrap());
        assert_eq!(dictionary.try_get_dict_as_map().unwrap().len(), 2);
        assert!(dictionary.try_get_has_key(b"/A").unwrap());
        assert!(!dictionary.try_get_has_key(b"/B").unwrap());
        let mut items = dictionary.try_dict_items().unwrap().begin();
        assert_eq!(items.current().key, b"/A");
        items.next();
        assert!(items.is_end());
    }

    #[test]
    fn dict_item_cursor_falls_back_to_uninitialized_when_the_container_stops_being_a_dictionary() {
        let dictionary = ObjectHandle::dictionary(vec![(b"/A".to_vec(), ObjectHandle::integer(1))]);
        let mut cursor = dictionary.try_dict_items().unwrap().begin();

        // The cursor keeps the same live dictionary handle rather than a
        // frozen snapshot; if it stops being a dictionary mid-iteration
        // (its Rc-shared slot is reassigned a scalar payload here), the
        // cursor must fall back rather than panic on a stale key lookup.
        dictionary
            .share_value_state_with(&ObjectHandle::integer(2))
            .expect("reassign the shared slot's payload to a non-dictionary value");

        let entry = cursor.current();
        assert_eq!(entry.key, b"/A");
        assert!(!entry.value.is_initialized());
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
    fn signed_array_mutators_cover_qpdf_bounds_and_success_paths() {
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1)]);
        array
            .try_set_array_item_at(0, ObjectHandle::integer(2))
            .unwrap();
        array
            .try_append_array_item(ObjectHandle::integer(3))
            .unwrap();
        array
            .try_set_array_items(vec![ObjectHandle::integer(4)])
            .unwrap();
        array
            .try_insert_array_item_at(1, ObjectHandle::integer(5))
            .unwrap();
        array.try_erase_array_item_at(0).unwrap();
        assert_eq!(array.as_array().unwrap().len(), 1);

        let (array, recorder) =
            handle_resolving(ObjectValue::Array(vec![ObjectHandle::integer(1)]));
        array
            .try_set_array_item_at(-1, ObjectHandle::integer(2))
            .unwrap();
        array
            .try_set_array_item_at(42, ObjectHandle::integer(2))
            .unwrap();
        array
            .try_insert_array_item_at(-1, ObjectHandle::integer(2))
            .unwrap();
        array
            .try_insert_array_item_at(42, ObjectHandle::integer(2))
            .unwrap();
        array.try_erase_array_item_at(-1).unwrap();
        array.try_erase_array_item_at(42).unwrap();
        assert_eq!(warnings(&recorder).len(), 6);

        let (integer, recorder) = handle_resolving(ObjectValue::Integer(7));
        integer
            .try_set_array_item_at(-1, ObjectHandle::null())
            .unwrap();
        integer
            .try_set_array_item_at(0, ObjectHandle::null())
            .unwrap();
        integer
            .try_insert_array_item_at(-1, ObjectHandle::null())
            .unwrap();
        integer
            .try_insert_array_item_at(0, ObjectHandle::null())
            .unwrap();
        integer.try_erase_array_item_at(-1).unwrap();
        integer.try_erase_array_item_at(0).unwrap();
        assert_eq!(warnings(&recorder).len(), 6);
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
    fn integer_size_accessors_match_qpdf_test_62() {
        // `qpdf/test_driver.cc:2263-2287` exercises the same values and
        // conversions. Keep each handle separate so the warning sequence
        // remains the sequence of calls made by qpdf's test.
        let q1_l = 3_u64 * u64::from(u32::try_from(i32::MAX).unwrap());
        let q1 = i64::try_from(q1_l).unwrap();
        let (q1_handle, q1_recorder) = handle_resolving(ObjectValue::Integer(q1));
        assert_eq!(q1_handle.try_get_int_value().unwrap(), q1);
        assert_eq!(q1_handle.try_get_uint_value().unwrap(), q1_l);
        assert_eq!(q1_handle.try_get_int_value_as_int().unwrap(), i32::MAX);
        assert_eq!(q1_handle.try_get_uint_value_as_uint().unwrap(), u32::MAX);
        assert_eq!(
            warnings(&q1_recorder),
            [
                "object 3 0: requested value of integer is too big; returning INT_MAX",
                "object 3 0: requested value of unsigned integer is too big; returning UINT_MAX",
            ]
        );

        let q2 = 3_i64 * i64::from(i32::MIN);
        let (q2_handle, q2_recorder) = handle_resolving(ObjectValue::Integer(q2));
        assert_eq!(q2_handle.try_get_int_value().unwrap(), q2);
        assert_eq!(q2_handle.try_get_uint_value().unwrap(), 0);
        assert_eq!(q2_handle.try_get_int_value_as_int().unwrap(), i32::MIN);
        assert_eq!(q2_handle.try_get_uint_value_as_uint().unwrap(), 0);
        assert_eq!(
            warnings(&q2_recorder),
            [
                "object 3 0: unsigned value request for negative number; returning 0",
                "object 3 0: requested value of integer is too small; returning INT_MIN",
                "object 3 0: unsigned integer value request for negative number; returning 0",
            ]
        );

        let (q3_handle, q3_recorder) = handle_resolving(ObjectValue::Integer(i64::from(u32::MAX)));
        assert_eq!(q3_handle.try_get_int_value_as_int().unwrap(), i32::MAX);
        assert_eq!(q3_handle.try_get_uint_value_as_uint().unwrap(), u32::MAX);
        assert_eq!(
            warnings(&q3_recorder),
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
            filter_on_write: true,
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
    fn nested_missing_key_warning_keeps_the_qpdf_document_context() {
        let (root, recorder) =
            handle_resolving(ObjectValue::Dictionary(std::collections::BTreeMap::new()));

        let pages = root.try_get_key(b"/Pages").unwrap();
        let count = pages.try_get_key(b"/Count").unwrap();

        assert_eq!(count.try_get_int_value().unwrap(), 0);
        assert_eq!(
            warnings(&recorder),
            [
                "object 3 0 -> dictionary key /Pages: operation for dictionary attempted on object of type null: returning null for attempted key retrieval",
                "object 3 0 -> dictionary key /Pages -> null returned from getting key  from non-Dictionary: operation for integer attempted on object of type null: returning 0",
            ]
        );
    }

    #[test]
    fn context_terminates_on_a_reciprocal_child_description_cycle() {
        let first = ObjectHandle::integer(1);
        let second = ObjectHandle::integer(2);
        first.set_child_description(&second, " -> first $VD", "/First");
        second.set_child_description(&first, " -> second $VD", "/Second");

        assert!(first.context().is_none());
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
    fn nested_lifted_nulls_keep_qpdfs_contextless_dictionary_contract() {
        fn assert_contextless_dictionary_access(handle: ObjectHandle) {
            let error = handle
                .try_get_keys()
                .expect_err("qpdf newNull has no owning document context");
            assert!(matches!(
                error,
                crate::Error::System(ref message)
                    if message
                        == "operation for dictionary attempted on object of type null: treating as empty"
            ));
        }

        let (array, array_recorder) =
            handle_resolving(ObjectValue::Array(vec![ObjectHandle::null()]));
        let array_null = array
            .try_array_item(0)
            .unwrap()
            .expect("array contains the lifted null");
        assert_contextless_dictionary_access(array_null);
        assert!(warnings(&array_recorder).is_empty());

        let (dictionary, dictionary_recorder) = handle_resolving(ObjectValue::Dictionary(
            [(b"/Null".to_vec(), ObjectHandle::null())]
                .into_iter()
                .collect(),
        ));
        let dictionary_null = dictionary.try_get_key(b"/Null").unwrap();
        assert_contextless_dictionary_access(dictionary_null);
        assert!(warnings(&dictionary_recorder).is_empty());

        let contextless_dictionary = ObjectHandle::dictionary(vec![]);
        let missing_key_null = contextless_dictionary.try_get_key(b"/Missing").unwrap();
        let error = missing_key_null
            .try_get_keys()
            .expect_err("qpdf missing-key null has no context without a document");
        assert!(matches!(
            error,
            crate::Error::System(ref message)
                if message
                    == " -> dictionary key /Missing: operation for dictionary attempted on object of type null: treating as empty"
        ));

        let (stream, stream_recorder) = handle_resolving(ObjectValue::Stream {
            stream_dict: ObjectHandle::dictionary(vec![(b"/Null".to_vec(), ObjectHandle::null())]),
            stream_data: Some(Rc::new(Vec::new())),
            stream_provider: None,
            filter_on_write: true,
            stream_length: 0,
        });
        stream.try_dereference().unwrap();
        let stream_null = stream
            .as_stream_dict()
            .expect("stream has a dictionary")
            .try_get_key(b"/Null")
            .unwrap();
        assert_contextless_dictionary_access(stream_null);
        assert!(warnings(&stream_recorder).is_empty());

        let (non_null, non_null_recorder) =
            handle_resolving(ObjectValue::Array(vec![ObjectHandle::integer(7)]));
        let non_null_child = non_null
            .try_array_item(0)
            .unwrap()
            .expect("array contains the integer");
        assert!(non_null_child.try_get_keys().unwrap().is_empty());
        assert_eq!(
            warnings(&non_null_recorder),
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

#[cfg(test)]
mod qpdf_mutator_api_tests {
    use super::*;

    #[test]
    fn and_get_dictionary_mutators_preserve_qpdf_order_and_identity() {
        let dict = ObjectHandle::dictionary(vec![]);
        let alias = dict.clone();

        let inserted = dict
            .replace_key_and_get_new(b"/Three", ObjectHandle::array(vec![]))
            .expect("replaceKeyAndGetNew should insert into a dictionary");
        assert!(inserted.is_same_object_as(&dict.get_key(b"/Three")));

        let old = dict
            .replace_key_and_get_old(b"/Three", ObjectHandle::integer(3))
            .expect("replaceKeyAndGetOld should replace a dictionary key");
        assert!(old.is_same_object_as(&inserted));
        assert_eq!(alias.get_key(b"/Three").as_integer(), Some(3));

        let missing = dict
            .remove_key_and_get_old(b"/Missing")
            .expect("removeKeyAndGetOld should return a null for a missing key");
        assert!(missing.is_null());
        let removed = dict
            .remove_key_and_get_old(b"/Three")
            .expect("removeKeyAndGetOld should return the existing value");
        assert_eq!(removed.as_integer(), Some(3));
        assert!(dict.get_key(b"/Three").is_null());
    }

    #[test]
    fn and_get_dictionary_mutators_keep_qpdf_type_warning_boundaries() {
        let (handle, recorder) = warning_emission_tests::handle_resolving(ObjectValue::Integer(7));

        let returned = handle
            .replace_key_and_get_new(b"/Ignored", ObjectHandle::integer(1))
            .expect("a contextful qpdf type warning is recoverable");
        assert_eq!(returned.as_integer(), Some(1));

        let old = handle
            .remove_key_and_get_old(b"/Ignored")
            .expect("removeKeyAndGetOld should return null after its warning");
        assert!(old.is_null());

        assert_eq!(
            warning_emission_tests::warnings(&recorder),
            [
                "object 3 0: operation for dictionary attempted on object of type integer: ignoring key replacement request",
                "object 3 0: operation for dictionary attempted on object of type integer: ignoring key removal request",
            ]
        );
    }

    #[test]
    fn set_object_description_routes_array_warning_through_the_document() {
        let pdf = crate::Pdf::empty().expect("empty PDF should be constructible");
        let array = ObjectHandle::array(vec![]);
        array
            .set_object_description(&pdf, "test array")
            .expect("live document context should be attachable");

        let erased = array
            .erase_array_item_and_get_old(50)
            .expect("qpdf reports an out-of-bounds erase as a warning");
        assert!(erased.is_null());
        assert_eq!(
            pdf.repair_diagnostics()
                .entries()
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            ["test array: ignoring attempt to erase out of bounds array item"]
        );
    }
}

#[cfg(test)]
mod filter_on_write_tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn filter_on_write_defaults_true_and_is_shared_by_aliases() {
        let stream =
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new()));
        let alias = stream.clone();

        assert!(
            stream
                .get_filter_on_write()
                .expect("stream getter should succeed"),
            "qpdf streams enable filtering by default"
        );

        stream
            .set_filter_on_write(false)
            .expect("stream setter should succeed");
        assert!(!alias
            .get_filter_on_write()
            .expect("alias getter should succeed"));

        alias
            .set_filter_on_write(true)
            .expect("alias setter should succeed");
        assert!(stream
            .get_filter_on_write()
            .expect("stream getter should observe alias mutation"));
    }

    #[test]
    fn filter_on_write_accessors_reject_non_stream_values() {
        let value = ObjectHandle::integer(7);

        assert!(matches!(
            value.set_filter_on_write(false),
            Err(crate::Error::System(message))
                if message == "operation for stream attempted on object of type integer"
        ));
        assert!(matches!(
            value.get_filter_on_write(),
            Err(crate::Error::System(message))
                if message == "operation for stream attempted on object of type integer"
        ));
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;
    use std::process::Command;
    use std::rc::Rc;

    const DEEP_DROP_DEPTH: usize = 100_000;

    fn assert_drop_probe_succeeds(test_name: &str, environment: &str) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--ignored", "--nocapture"])
            .env(environment, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "drop probe failed: status={} stderr={}",
            output.status,
            stderr
        );
    }

    #[test]
    fn deep_direct_array_drop_is_stack_independent() {
        assert_drop_probe_succeeds(
            "object_handle::drop_tests::deep_direct_array_drop_probe",
            "FLPDF_DEEP_DIRECT_ARRAY_DROP_PROBE",
        );
    }

    #[test]
    #[ignore = "subprocess-only stack-overflow regression probe"]
    fn deep_direct_array_drop_probe() {
        assert_eq!(
            std::env::var_os("FLPDF_DEEP_DIRECT_ARRAY_DROP_PROBE").as_deref(),
            Some(std::ffi::OsStr::new("1"))
        );

        let mut handle = ObjectHandle::integer(0);
        for _ in 0..DEEP_DROP_DEPTH {
            handle = ObjectHandle::array(vec![handle]);
        }
        drop(handle);
    }

    #[test]
    fn deep_direct_dictionary_drop_is_stack_independent() {
        assert_drop_probe_succeeds(
            "object_handle::drop_tests::deep_direct_dictionary_drop_probe",
            "FLPDF_DEEP_DIRECT_DICTIONARY_DROP_PROBE",
        );
    }

    #[test]
    #[ignore = "subprocess-only stack-overflow regression probe"]
    fn deep_direct_dictionary_drop_probe() {
        assert_eq!(
            std::env::var_os("FLPDF_DEEP_DIRECT_DICTIONARY_DROP_PROBE").as_deref(),
            Some(std::ffi::OsStr::new("1"))
        );

        let mut handle = ObjectHandle::integer(0);
        for _ in 0..DEEP_DROP_DEPTH {
            handle = ObjectHandle::dictionary(vec![(b"/Next".to_vec(), handle)]);
        }
        drop(handle);
    }

    #[test]
    fn deep_direct_stream_dictionary_drop_is_stack_independent() {
        assert_drop_probe_succeeds(
            "object_handle::drop_tests::deep_direct_stream_dictionary_drop_probe",
            "FLPDF_DEEP_DIRECT_STREAM_DROP_PROBE",
        );
    }

    #[test]
    #[ignore = "subprocess-only stack-overflow regression probe"]
    fn deep_direct_stream_dictionary_drop_probe() {
        assert_eq!(
            std::env::var_os("FLPDF_DEEP_DIRECT_STREAM_DROP_PROBE").as_deref(),
            Some(std::ffi::OsStr::new("1"))
        );

        let data = Rc::new(Vec::new());
        let mut handle = ObjectHandle::integer(0);
        for _ in 0..DEEP_DROP_DEPTH {
            let dictionary = ObjectHandle::dictionary(vec![(b"/Next".to_vec(), handle)]);
            handle = ObjectHandle::stream(dictionary, Rc::clone(&data));
        }
        drop(handle);
        assert_eq!(Rc::strong_count(&data), 1);
    }

    #[test]
    fn dropping_a_parent_does_not_dismantle_a_shared_child_alias() {
        let child = ObjectHandle::dictionary(vec![(b"/Value".to_vec(), ObjectHandle::integer(7))]);
        let alias = child.clone();
        let parent = ObjectHandle::array(vec![child]);

        drop(parent);

        assert_eq!(alias.get_key(b"/Value").as_integer(), Some(7));
        assert!(alias.containing_object_refs().is_empty());
        drop(alias);
    }

    #[test]
    fn an_identity_key_can_be_the_last_owner_of_a_direct_container() {
        let root_ref = ObjectRef::new(12, 0);
        let leaf = ObjectHandle::integer(3);
        let root = ObjectHandle::new_indirect_unresolved(root_ref, -1);
        root.set_resolved(ObjectValue::Array(vec![leaf.clone()]));
        let identity = root.identity_key();

        drop(root);
        assert_eq!(leaf.containing_object_refs(), vec![root_ref]);

        drop(identity);
        assert!(leaf.containing_object_refs().is_empty());
    }
}
