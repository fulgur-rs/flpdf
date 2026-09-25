//! Serialize PDF objects with writer-owned emission state.
//!
//! qpdf correspondence: `QPDFWriter::unparseObject`, `unparseChild`, `writeTrailer`, and the writer-owned live-handle emission boundary.
//!
//! qpdf sources: `libqpdf/QPDFWriter.cc:1072-1810,2236-2376,2907-3035`.
//!
//! The object graph remains owned by [`crate::ObjectHandle`].  This module
//! owns traversal, output-reference remapping, null visibility, QDF framing,
//! and emission-time string policy.  Keeping this boundary in `writer/`
//! prevents the object model from growing a second writer responsibility.
//!
//! Every entry point here is named after qpdf's `unparseObject` (value-only
//! serialization) or `writeTrailer`, never after `writeObject`: the indirect
//! object envelope (`N 0 obj` … `endobj`) that qpdf's `writeObject` adds
//! around an `unparseObject` call belongs to
//! [`crate::writer::write_object::WriteObject::write_object`] instead.

use super::output::{
    decimal_u64_len, write_decimal_i64, write_decimal_u64, write_object_ref, OutputSink,
};
use crate::object_handle::{LiveDictionaryKeyBuffer, ObjectHandle, ObjectValue};
use crate::qpdf_obj_gen::QpdfObjGen;
use crate::{Error, ObjectRef, Result};
use std::collections::BTreeSet;

/// qpdf's `writeTrailer` form selector (`QPDFWriter.cc:1160-1236`).
#[allow(dead_code)] // linearized consumers are migrated in the following D14 route slices
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrailerKind {
    Normal { size: i64 },
    LinearizedFirst { size: i64, prev: u64 },
    LinearizedSecond { size: i64 },
}

/// qpdf's `getTrimmedTrailer` structural-key removal set
/// (`libqpdf/QPDFWriter.cc:2009-2031`): keys the writer always supplies
/// itself (encryption/modification metadata plus every key that may
/// originate from a source cross-reference stream), so a source trailer's
/// own copy must never reach a trailer-entries walk. Every production
/// consumer of the live source trailer -- `crate::writer::build_writer_trailer_handle`
/// (classic table and xref-stream routes, which removes these keys in
/// place before [`write_trailer_with_ref_map_and_kind_and_direct_root`]
/// walks the result) and
/// `crate::linearization::writer::canonical_linearization_trailer_entries`
/// (the linearized two-pass route, which filters them out while walking
/// since it cannot mutate a shared live handle mid-pass) -- shares this one
/// list rather than each retyping qpdf's removal set independently.
pub(crate) const TRIMMED_TRAILER_KEYS: &[&[u8]] = &[
    b"/ID",
    b"/Encrypt",
    b"/Prev",
    b"/Index",
    b"/W",
    b"/Length",
    b"/Filter",
    b"/DecodeParms",
    b"/Type",
    b"/XRefStm",
];

/// Stream-dictionary changes selected by qpdf's `willFilterStream` result.
///
/// `remove_filter_parameters` corresponds to `f_filtered` in
/// `QPDFWriter::unparseObject`: it removes only `/Filter` and `/DecodeParms`
/// from the shallow dictionary copy. `add_flate_filter` is the independent
/// `compress && f_filtered` tail. Keeping these bits separate matters for
/// uncompressing and metadata streams, where qpdf removes the source filter
/// parameters without appending a new Flate filter
/// (`libqpdf/QPDFWriter.cc:1274-1284,1440-1455,1508-1522`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamDictionaryOptions {
    pub(crate) remove_filter_parameters: bool,
    pub(crate) add_flate_filter: bool,
}

impl StreamDictionaryOptions {
    pub(crate) const fn new(remove_filter_parameters: bool, add_flate_filter: bool) -> Self {
        Self {
            remove_filter_parameters,
            add_flate_filter,
        }
    }

    pub(crate) const fn preserve() -> Self {
        Self::new(false, false)
    }

    pub(crate) const fn from_refiltered(refiltered: bool) -> Self {
        Self::new(refiltered, refiltered)
    }
}

/// The single writer-owned emission surface for live `ObjectHandle` values.
///
/// This is deliberately crate-private: it is the writer-owned replacement for
/// the removed handle-owned emission route. All implementations and callers
/// live at the writer boundary, while the handle itself retains only graph
/// identity, payload, and mutation responsibilities.
#[allow(dead_code)]
pub(crate) trait ObjectWriterEmission {
    fn unparse_object(&self, out: &mut OutputSink<'_>) -> Result<()>;
    #[cfg(test)]
    fn unparse_object_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn unparse_object_qdf(&self, out: &mut OutputSink<'_>, indent: usize) -> Result<()>;
    fn unparse_object_qdf_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn unparse_object_qdf_with_qpdf_obj_gen_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()>;
    #[cfg(test)]
    fn unparse_object_qdf_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    fn unparse_object_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn unparse_object_with_qpdf_obj_gen_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()>;
    #[cfg(test)]
    fn unparse_root_object_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        final_pdf_version: &str,
        final_extension_level: i64,
        apply_adbe_reconciliation: bool,
    ) -> Result<()>;
    // cov:ignore-start: only the live ObjectHandle owner implements this
    // surface; planned emitters never call the defensive defaults.
    fn output_root_copy_with_adbe(
        &self,
        final_pdf_version: &str,
        final_extension_level: i64,
        apply_adbe_reconciliation: bool,
    ) -> Result<ObjectHandle> {
        let _ = (
            final_pdf_version,
            final_extension_level,
            apply_adbe_reconciliation,
        );
        Err(Error::Internal(
            "root output copy is unavailable for this emission owner".into(),
        ))
    }
    /// Live-queue emission variant: the callback receives the complete child
    /// handle before its output reference is assigned, so owner checks and
    /// first-seen queue insertion happen at qpdf's `unparseChild` boundary.
    fn unparse_object_with_dynamic_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        let _ = (out, map, removed_refs);
        Err(Error::Internal(
            "dynamic writer map is unavailable for this emission owner".into(),
        ))
    }
    fn unparse_root_object_with_dynamic_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        final_pdf_version: &str,
        final_extension_level: i64,
        apply_adbe_reconciliation: bool,
    ) -> Result<()> {
        let _ = (
            out,
            map,
            removed_refs,
            final_pdf_version,
            final_extension_level,
            apply_adbe_reconciliation,
        );
        Err(Error::Internal(
            "dynamic root writer map is unavailable for this emission owner".into(),
        ))
    }
    fn unparse_stream_body_with_dynamic_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        let _ = (out, options, map, removed_refs);
        Err(Error::Internal(
            "dynamic stream writer map is unavailable for this emission owner".into(),
        ))
    }
    #[allow(clippy::type_complexity)]
    fn unparse_stream_body_with_dynamic_ref_map_and_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
        direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized;
    // cov:ignore-end
    fn unparse_object_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    fn unparse_object_with_qpdf_obj_gen_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    fn unparse_object_qdf_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    fn unparse_object_qdf_with_qpdf_obj_gen_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;

    fn unparse_stream_body(&self, out: &mut OutputSink<'_>, refiltered: bool) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_qdf(&self, out: &mut OutputSink<'_>, indent: usize) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> Result<()>;
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
    ) -> Result<()>;
    fn unparse_stream_body_qdf_with_qpdf_obj_gen_map_and_removed_and_length_with_options(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
    ) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    fn unparse_stream_body_qdf_with_qpdf_obj_gen_map_and_removed_and_length_with_string_writer_with_options<
        F,
    >(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer_with_options<
        F,
    >(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_qdf_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn unparse_stream_body_with_ref_map_and_removed_with_options(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()>;
    /// Linearized stream-dictionary emission with an output payload-length
    /// override. The source dictionary's top-level keys remain unchanged,
    /// matching qpdf's writer-side stream length calculation; nested filter
    /// arrays retain the existing qpdf shallow-copy alias cleanup semantics.
    fn unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        length: usize,
    ) -> Result<()>;
    /// Stream-dictionary emission with a direct output `/Length` override.
    /// The source handle remains unchanged; this mirrors qpdf's stream writer,
    /// which computes the emitted length from the bytes supplied to its pipe.
    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed_and_length(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length: usize,
    ) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed_and_length_with_options(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length: usize,
    ) -> Result<()>;
    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    fn unparse_stream_body_with_ref_map_and_removed_with_options_and_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;
    fn unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>;

    #[cfg(test)]
    fn write_trailer(
        &self,
        out: &mut OutputSink<'_>,
        xref_stream: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    ) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    fn write_trailer_with_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        xref_stream: bool,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()>;
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::type_complexity)] // test-only legacy adapter mirrors the qpdf trailer callback boundary
    fn write_trailer_with_ref_map_and_kind(
        &self,
        out: &mut OutputSink<'_>,
        kind: TrailerKind,
        xref_stream: bool,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()>;
    #[cfg(test)]
    fn write_dictionary_with_ref_map_and_id_writer(
        &self,
        out: &mut OutputSink<'_>,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
    ) -> Result<()>;
    fn write_id_value_with_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn write_id_value_with_qpdf_obj_gen_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()>;
}

/// Vec-backed adapter used only by legacy serializer unit tests.
///
/// Production callers have one canonical [`OutputSink`] surface. These
/// wrappers keep existing byte assertions compact while ensuring every test
/// invocation still traverses that sink surface.
#[cfg(test)]
pub(crate) trait ObjectWriterEmissionVecTestExt {
    fn unparse_object(&self, out: &mut Vec<u8>) -> Result<()>;
    fn unparse_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()>;
    fn unparse_object_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn unparse_object_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn unparse_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn unparse_stream_body(&self, out: &mut Vec<u8>, refiltered: bool) -> Result<()>;
    fn unparse_stream_body_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn unparse_stream_body_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()>;
    fn unparse_stream_body_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn unparse_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()>;
    fn unparse_stream_body_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> Result<()>;
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>;
    #[allow(clippy::type_complexity)] // test-only legacy adapter mirrors the qpdf trailer callback boundary
    fn write_trailer(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        id_writer: Option<&mut dyn FnMut(&mut Vec<u8>)>,
    ) -> Result<()>;
}

#[cfg(test)]
impl ObjectWriterEmissionVecTestExt for ObjectHandle {
    fn unparse_object(&self, out: &mut Vec<u8>) -> Result<()> {
        super::output::with_buffer_sink(out, |out| ObjectWriterEmission::unparse_object(self, out))
    }

    fn unparse_object_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_object_qdf(self, out, indent)
        })
    }

    fn unparse_object_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        let mut sink_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            let mut bytes = Vec::new();
            write_string(&mut bytes, value)?;
            out.write_bytes(&bytes)
        };
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_object_with_string_writer(self, out, &mut sink_writer)
        })
    }

    fn unparse_object_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        let mut sink_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            let mut bytes = Vec::new();
            write_string(&mut bytes, value)?;
            out.write_bytes(&bytes)
        };
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_object_qdf_with_string_writer(
                self,
                out,
                indent,
                &mut sink_writer,
            )
        })
    }

    fn unparse_object_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_object_with_ref_map_and_removed(
                self,
                out,
                map,
                removed_refs,
            )
        })
    }

    fn unparse_stream_body(&self, out: &mut Vec<u8>, refiltered: bool) -> Result<()> {
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body(self, out, refiltered)
        })
    }

    fn unparse_stream_body_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        let mut sink_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            let mut bytes = Vec::new();
            write_string(&mut bytes, value)?;
            out.write_bytes(&bytes)
        };
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_with_string_writer(
                self,
                out,
                refiltered,
                &mut sink_writer,
            )
        })
    }

    fn unparse_stream_body_qdf(&self, out: &mut Vec<u8>, indent: usize) -> Result<()> {
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_qdf(self, out, indent)
        })
    }

    fn unparse_stream_body_qdf_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        let mut sink_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            let mut bytes = Vec::new();
            write_string(&mut bytes, value)?;
            out.write_bytes(&bytes)
        };
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_qdf_with_string_writer(
                self,
                out,
                indent,
                &mut sink_writer,
            )
        })
    }

    fn unparse_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_with_ref_map_and_removed(
                self,
                out,
                refiltered,
                map,
                removed_refs,
            )
        })
    }

    fn unparse_stream_body_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        let mut sink_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            let mut bytes = Vec::new();
            write_string(&mut bytes, value)?;
            out.write_bytes(&bytes)
        };
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_with_ref_map_and_removed_with_string_writer(
                self,
                out,
                refiltered,
                map,
                removed_refs,
                &mut sink_writer,
            )
        })
    }

    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> Result<()> {
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
                self,
                out,
                indent,
                map,
                removed_refs,
                length_ref,
            )
        })
    }

    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer<F>(
        &self,
        out: &mut Vec<u8>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut Vec<u8>, &[u8]) -> Result<()>,
    {
        let mut sink_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            let mut bytes = Vec::new();
            write_string(&mut bytes, value)?;
            out.write_bytes(&bytes)
        };
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer(self, out, indent, map, removed_refs, length_ref, &mut sink_writer)
        })
    }

    fn write_trailer(
        &self,
        out: &mut Vec<u8>,
        xref_stream: bool,
        mut id_writer: Option<&mut dyn FnMut(&mut Vec<u8>)>,
    ) -> Result<()> {
        let has_id_writer = id_writer.is_some();
        let mut sink_writer = |out: &mut OutputSink<'_>| {
            let mut bytes = Vec::new();
            if let Some(write_id) = id_writer.as_deref_mut() {
                write_id(&mut bytes);
            }
            out.write_bytes(&bytes)
        };
        let sink_writer =
            has_id_writer.then_some(&mut sink_writer as crate::pdf_syntax::TrailerIdWriter<'_>);
        super::output::with_buffer_sink(out, |out| {
            ObjectWriterEmission::write_trailer(self, out, xref_stream, sink_writer)
        })
    }
}

const UNPARSE_STACK_RED_ZONE: usize = 32 * 1024;
const UNPARSE_STACK_GROWTH_SIZE: usize = 1024 * 1024;

// Active recursion depth of this module's `unparse_object_walk*` family.
//
// `stacker::maybe_grow` swaps stack segments without leaving the current
// thread, so a thread-local counter observes every level of one walk and
// never mixes two concurrent writes.
//
// qpdf's `QPDFWriter::unparseObject` (`libqpdf/QPDFWriter.cc:1318-1325`)
// validates only that its `level` is non-negative. Nothing bounds the walk
// from above and nothing records the nodes already on the path, so a pair of
// *direct* dictionaries holding each other recurses until the process runs
// out of memory. Parsed input cannot reach that shape -- `parser.rs` caps
// container nesting at `MAX_PARSE_DEPTH` and the direct containers it builds
// are trees -- but `ObjectHandle::replace_key` accepts it from a library
// caller, since it refuses only the single-hop self-insert and not the
// two-hop pair `a.replace_key(b"/B", b)` plus `b.replace_key(b"/A", a)`.
// Bounding every hub at the parser's own limit turns such a graph into a
// diagnostic instead of resource exhaustion, and leaves every graph the
// parser can produce untouched. The live body writer's direct-seed collector
// (`writer/plain/body.rs`) already refuses the same nesting before emission
// starts, so this makes the remaining emission routes agree with it.
//
// The count starts at zero for the outermost hub, so `MAX_PARSE_DEPTH + 1`
// hub levels are admitted rather than exactly `MAX_PARSE_DEPTH`. That extra
// level is not slack: a top-level indirect stream spends one hub on the
// stream itself and a second on its dictionary (`UnparseContainer::Stream`
// re-enters the walk with `stream_dict`), so a maximally nested parsed
// stream dictionary would otherwise trip a bound set at the parser's count.
thread_local! {
    static UNPARSE_WALK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// Counts one active level of the walk and restores the count when the level
// unwinds, on the error path as well as the success path.
struct UnparseWalkDepthGuard;

impl UnparseWalkDepthGuard {
    #[deprecated(
        note = "no qpdf counterpart; QPDFWriter::unparseObject has no upper nesting limit"
    )]
    fn enter() -> Result<Self> {
        let depth = UNPARSE_WALK_DEPTH.with(|depth| {
            let entered = depth.get();
            depth.set(entered + 1);
            entered
        });
        // Constructed before the bound is tested so the count is restored
        // even when this level is rejected.
        let guard = Self;
        if depth > crate::parser::MAX_PARSE_DEPTH {
            return Err(Error::Unsupported(format!(
                "writer: direct object nesting exceeds maximum of {}",
                crate::parser::MAX_PARSE_DEPTH
            )));
        }
        Ok(guard)
    }
}

impl Drop for UnparseWalkDepthGuard {
    fn drop(&mut self) {
        UNPARSE_WALK_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Run one level of an `unparse_object_walk*` recursion with the family's
/// shared stack growth and nesting bound.
///
/// Every hub in the family routes its body through here, so a direct
/// container graph that never reaches an indirect boundary is rejected at
/// the same depth wherever it is met.
#[allow(deprecated)]
fn unparse_object_walk_hub<T>(body: impl FnOnce() -> Result<T>) -> Result<T> {
    let _depth = UnparseWalkDepthGuard::enter()?;
    stacker::maybe_grow(UNPARSE_STACK_RED_ZONE, UNPARSE_STACK_GROWTH_SIZE, body)
}

fn reserved_unparse_error() -> Error {
    Error::System("QPDFObjectHandle: attempting to unparse a reserved object".to_owned())
}

fn unresolved_unparse_error() -> Error {
    Error::Internal("attempted to unparse an unresolved QPDFObjectHandle".to_owned())
}

fn destroyed_unparse_error() -> Error {
    Error::Internal("attempted to unparse a QPDFObjectHandle from a destroyed QPDF".to_owned())
}

fn write_dictionary_key(out: &mut OutputSink<'_>, key: &[u8]) -> Result<()> {
    if let Some(key) = key.strip_prefix(b"/") {
        out.write_bytes(b"/")?;
        crate::pdf_syntax::write_name_escaped(out, key)?;
    } else {
        // QPDF_Name::normalizeName preserves the first byte of a raw qpdf
        // dictionary key (`libqpdf/QPDF_Name.cc:27-50`). In particular,
        // `replaceKey("Array1", ...)` is intentionally emitted as the
        // slashless token `Array1`; do not silently canonicalize it here.
        crate::pdf_syntax::write_name_escaped(out, key)?;
    }
    Ok(())
}

fn write_qpdf_object_gen(out: &mut OutputSink<'_>, object_gen: QpdfObjGen) -> Result<()> {
    write_decimal_i64(out, i64::from(object_gen.get_obj()))?;
    out.write_bytes(b" ")?;
    write_decimal_i64(out, i64::from(object_gen.get_gen()))?;
    out.write_bytes(b" R")
}

impl ObjectWriterEmission for ObjectHandle {
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
    /// does not go through [`unparse_child`]'s indirect-reference check the
    /// way a *child* position would) and inlines just the stream's
    /// dictionary — `<< ... >>` with no `stream`/`endstream` framing and no
    /// `/Length`-last repositioning. That is not what qpdf's real
    /// stream-writing call produces at this position; this primitive simply
    /// does not implement qpdf's stream-writing path
    /// (`QPDFWriter::unparseObject` entered with `f_stream` flags). The
    /// dedicated primitive for that is `unparse_stream_body`, which current
    /// writer routes call when stream framing is required; calling
    /// `unparse_object` directly on a stream-resolving handle
    /// is an underspecified, undocumented-by-qpdf shape whose current output
    /// is pinned, in `unparse_object_tests`, by
    /// `unparse_object_on_an_indirect_handle_resolving_to_a_stream_inlines_the_dictionary`
    /// rather than derived from any qpdf oracle.
    fn unparse_object(&self, out: &mut OutputSink<'_>) -> Result<()> {
        unparse_object_walk(self, out)
    }

    /// Writer-emission counterpart of [`Self::unparse_object`] that routes
    /// every ordinary direct PDF string through `write_string`. The qpdf
    /// signature `/Contents` exception remains cleartext hexadecimal because
    /// `QPDFWriter.cc:1501` supplies `f_hex_string | f_no_encryption`; it is
    /// therefore intentionally not sent to the callback. This is the
    /// emission-time hook used by qpdf's encrypted `unparseObject` branch
    /// (`QPDFWriter.cc:1567-1599`): containers and indirect child identity
    /// remain owned by `ObjectHandle`, while the caller supplies only the
    /// string representation policy.
    #[cfg(test)]
    fn unparse_object_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_with_string_writer(self, out, write_string)
    }

    /// QDF-mode counterpart of [`Self::unparse_object`] — same qpdf function
    /// and the same call shape (`QPDFWriter::unparseObject`,
    /// `QPDFWriter.cc:1318-1527`, `level=0, flags=0`), but with the writer's
    /// own `m->qdf_mode` member set to `true` rather than `false` — a mode
    /// flag `unparseObject` checks internally, not an alternate set of call
    /// arguments. Carries forward this port's existing split between compact
    /// and QDF container framing rather than re-deriving the indent
    /// arithmetic from scratch: `indent` is the column (number of leading
    /// spaces) at which *this* value's own opening delimiter sits, an array
    /// or dictionary's children are written at `indent + 2`, and its closing
    /// delimiter (`]` / `>>`) returns to column `indent` on its own line —
    /// exactly the established qpdf-shaped contract. Every scalar (including
    /// a resolved indirect handle) writes byte-identically to the non-QDF form; only array,
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
    #[cfg(test)]
    fn unparse_object_qdf(&self, out: &mut OutputSink<'_>, indent: usize) -> Result<()> {
        unparse_object_walk_qdf(self, indent, out)
    }

    /// QDF writer emission with output-reference remapping and qpdf null
    /// visibility for references removed during this write.
    fn unparse_object_qdf_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_object_walk_qdf_with_ref_map(self, indent, out, &map, &removed_refs)
    }

    fn unparse_object_qdf_with_qpdf_obj_gen_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()> {
        unparse_object_walk_qdf_with_ref_map(self, indent, out, map, removed_refs)
    }

    /// QDF-mode counterpart of [`Self::unparse_object_with_string_writer`],
    /// including its cleartext hexadecimal signature `/Contents` exception.
    #[cfg(test)]
    fn unparse_object_qdf_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_qdf_with_string_writer(self, indent, out, write_string)
    }

    /// Writer-emission counterpart that additionally treats references in
    /// `removed_refs` as qpdf nulls. This is the canonical equivalent of
    /// `renumber_qpdf_refs_in_place_with_removed` for live handle graphs: an
    /// array keeps the position as `null`, while dictionary visibility drops
    /// the null-valued key.
    fn unparse_object_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_object_walk_with_ref_map(self, out, &map, &removed_refs)
    }

    fn unparse_object_with_qpdf_obj_gen_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()> {
        unparse_object_walk_with_ref_map(self, out, map, removed_refs)
    }

    /// Emit the root through qpdf's output-only `unparseObject` mutation.
    ///
    /// qpdf makes an unsafe shallow copy of the root dictionary before
    /// reconciling `/Extensions /ADBE` (`QPDFWriter.cc:1347-1435`). Keep that
    /// copy local to serialization. Existing direct Extensions remain shared:
    /// replacing or removing ADBE there also changes the live graph. Creating
    /// or removing the root's Extensions key changes only the output copy.
    #[cfg(test)]
    fn unparse_root_object_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        final_pdf_version: &str,
        final_extension_level: i64,
        apply_adbe_reconciliation: bool,
    ) -> Result<()> {
        let root = root_output_copy_with_adbe(
            self,
            final_pdf_version,
            final_extension_level,
            apply_adbe_reconciliation,
        )?; // cov:ignore: LLVM attributes this fallible root-copy call terminator to an uncovered continuation; the helper's success and error paths are covered by the root emission tests.
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_object_walk_with_ref_map(&root, out, &map, &removed_refs)
    }

    fn unparse_object_with_dynamic_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        unparse_object_walk_with_dynamic_ref_map(self, out, map, removed_refs)
    }

    fn unparse_root_object_with_dynamic_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        final_pdf_version: &str,
        final_extension_level: i64,
        apply_adbe_reconciliation: bool,
    ) -> Result<()> {
        let root = root_output_copy_with_adbe(
            self,
            final_pdf_version,
            final_extension_level,
            apply_adbe_reconciliation,
        )?; // cov:ignore: LLVM attributes this fallible root-copy call terminator to an uncovered continuation; the helper's success and error paths are covered by the live-queue root emission tests.
        unparse_object_walk_with_dynamic_ref_map(&root, out, map, removed_refs)
    }

    fn output_root_copy_with_adbe(
        &self,
        final_pdf_version: &str,
        final_extension_level: i64,
        apply_adbe_reconciliation: bool,
    ) -> Result<ObjectHandle> {
        root_output_copy_with_adbe(
            self,
            final_pdf_version,
            final_extension_level,
            apply_adbe_reconciliation,
        )
    }

    fn unparse_stream_body_with_dynamic_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_with_dynamic_ref_map(&entries, options, out, map, removed_refs)
    }

    fn unparse_stream_body_with_dynamic_ref_map_and_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &mut dyn FnMut(&ObjectHandle) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
        direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
    {
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_with_dynamic_ref_map_and_string_writer(
            &entries,
            options,
            out,
            map,
            removed_refs,
            write_string,
            direct_stream_writer,
        )
    }

    /// Encrypted writer counterpart of
    /// [`Self::unparse_object_with_ref_map_and_removed`]. Reference identity,
    /// qpdf null visibility, and string encryption are all applied while the
    /// live handle graph is walked.
    fn unparse_object_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_object_walk_with_ref_map_and_string_writer(
            self,
            out,
            &map,
            &removed_refs,
            write_string,
        )
    }

    fn unparse_object_with_qpdf_obj_gen_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_with_ref_map_and_string_writer(
            self,
            out,
            map,
            removed_refs,
            write_string,
        )
    }

    /// QDF/encrypted counterpart of
    /// [`Self::unparse_object_with_ref_map_and_removed_with_string_writer`].
    fn unparse_object_qdf_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_object_walk_qdf_with_ref_map_and_string_writer(
            self,
            indent,
            out,
            &map,
            &removed_refs,
            write_string,
        )
    }

    fn unparse_object_qdf_with_qpdf_obj_gen_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        unparse_object_walk_qdf_with_ref_map_and_string_writer(
            self,
            indent,
            out,
            map,
            removed_refs,
            write_string,
        )
    }
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
    /// [`Self::unparse_object`]'s own top-level entry point -- this primitive's
    /// usual caller already
    /// holds an already-resolved stream's dictionary handle, but nothing
    /// enforces that at the type level, and an as-yet-unresolved indirect
    /// handle whose document has been dropped must surface as an error
    /// here too, not silently degrade to an empty `<< >>` the way an
    /// unresolved [`Self::with_value`] read alone would (see
    /// `unparse_stream_body_propagates_a_dropped_document_error`, which
    /// fails without this call).
    fn unparse_stream_body(&self, out: &mut OutputSink<'_>, refiltered: bool) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries(
            &entries,
            StreamDictionaryOptions::from_refiltered(refiltered),
            out,
        )
    }

    /// Stream-dictionary counterpart of
    /// [`Self::unparse_stream_body`] that routes ordinary direct PDF strings
    /// through `write_string` while retaining qpdf's `/Length` and refilter
    /// ordering. A signature `/Contents` value remains cleartext hexadecimal,
    /// matching qpdf's `f_hex_string | f_no_encryption` flags.
    #[cfg(test)]
    fn unparse_stream_body_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_with_string_writer(&entries, refiltered, out, write_string)
    }

    /// QDF-mode counterpart of [`Self::unparse_stream_body`] -- same
    /// delegation-target dimension as the QDF object writer is to
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
    /// rule as the QDF object writer and [`Self::unparse_stream_body`],
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
    #[cfg(test)]
    fn unparse_stream_body_qdf(&self, out: &mut OutputSink<'_>, indent: usize) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_qdf(&entries, indent, out)
    }

    #[cfg(test)]
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
    ) -> Result<()> {
        self.unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
            out,
            indent,
            map,
            removed_refs,
            length_ref,
            StreamDictionaryOptions::preserve(),
        )
    }

    /// QDF stream-dictionary emission with the same reference remapping and
    /// null visibility rules, but with an optional synthetic `/Length`
    /// reference. QDF full-rewrite streams do not retain the source length;
    /// qpdf writes a fresh holder immediately after the stream body. Keeping
    /// that override at the serializer boundary avoids manufacturing a fake
    /// source handle for an output-only object number.
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_stream_dict_entries_qdf_with_ref_map(
            &entries,
            indent,
            out,
            &map,
            &removed_refs,
            length_ref,
            options,
        )
    }

    fn unparse_stream_body_qdf_with_qpdf_obj_gen_map_and_removed_and_length_with_options(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_qdf_with_ref_map(
            &entries,
            indent,
            out,
            map,
            removed_refs,
            length_ref,
            options,
        )
    }

    #[cfg(test)]
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        self.unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer_with_options(
            out,
            indent,
            map,
            removed_refs,
            length_ref,
            StreamDictionaryOptions::preserve(),
            write_string,
        )
    }

    /// QDF stream-dictionary emission that combines output-reference
    /// remapping, removed-reference null visibility, and encrypted string
    /// serialization.
    fn unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer_with_options<
        F,
    >(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_stream_dict_entries_qdf_with_ref_map_and_string_writer(
            &entries,
            indent,
            out,
            &map,
            &removed_refs,
            length_ref,
            options,
            write_string,
        )
    }

    fn unparse_stream_body_qdf_with_qpdf_obj_gen_map_and_removed_and_length_with_string_writer_with_options<
        F,
    >(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        length_ref: Option<ObjectRef>,
        options: StreamDictionaryOptions,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_qdf_with_ref_map_and_string_writer(
            &entries,
            indent,
            out,
            map,
            removed_refs,
            length_ref,
            options,
            write_string,
        )
    }

    /// QDF-mode stream-dictionary counterpart of
    /// [`Self::unparse_stream_body_with_string_writer`], including its
    /// cleartext hexadecimal signature `/Contents` exception.
    #[cfg(test)]
    fn unparse_stream_body_qdf_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        indent: usize,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
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
                Some(ObjectValue::Stream(stream)) => {
                    stream.stream_dict.try_dereference()?;
                    stream
                        .stream_dict
                        .with_value(|dict_value| match dict_value {
                            Some(ObjectValue::Dictionary(entries)) => entries
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect(),
                            _ => Vec::new(),
                        })
                }
                _ => Vec::new(),
            };
            unparse_stream_dict_entries_qdf_with_string_writer(&entries, indent, out, write_string)
        })
    }

    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        self.unparse_stream_body_with_ref_map_and_removed_with_options(
            out,
            StreamDictionaryOptions::from_refiltered(refiltered),
            map,
            removed_refs,
        )
    }

    /// Stream-dictionary writer emission with output reference remapping and
    /// qpdf null visibility for references removed during this write.
    fn unparse_stream_body_with_ref_map_and_removed_with_options(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_stream_dict_entries_with_ref_map(&entries, options, out, &map, &removed_refs)
    }

    fn unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_with_ref_map(&entries, options, out, map, removed_refs)
    }

    fn unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        length: usize,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        self.try_dereference()?;
        let is_dictionary =
            self.with_value(|value| matches!(value, Some(ObjectValue::Dictionary(_))));
        if !is_dictionary {
            let entries = stream_dictionary_entries_for_emission(self)?;
            return unparse_stream_dict_entries_with_ref_map_and_length(
                &entries,
                options,
                out,
                map,
                removed_refs,
                Some(length),
            );
        }
        unparse_stream_dictionary_live_with_qpdf_obj_gen_map_and_removed_and_length(
            self,
            options,
            out,
            map,
            removed_refs,
            length,
        )
    }

    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed_and_length(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length: usize,
    ) -> Result<()> {
        self.unparse_stream_body_with_ref_map_and_removed_and_length_with_options(
            out,
            StreamDictionaryOptions::from_refiltered(refiltered),
            map,
            removed_refs,
            length,
        )
    }

    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed_and_length_with_options(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        length: usize,
    ) -> Result<()> {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_stream_dict_entries_with_ref_map_and_length(
            &entries,
            options,
            out,
            &map,
            &removed_refs,
            Some(length),
        )
    }

    #[cfg(test)]
    fn unparse_stream_body_with_ref_map_and_removed_with_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        refiltered: bool,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        self.unparse_stream_body_with_ref_map_and_removed_with_options_and_string_writer(
            out,
            StreamDictionaryOptions::from_refiltered(refiltered),
            map,
            removed_refs,
            write_string,
        )
    }

    /// Compact stream-dictionary emission with output-reference remapping,
    /// removed-reference null visibility, and encrypted string serialization.
    fn unparse_stream_body_with_ref_map_and_removed_with_options_and_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        unparse_stream_dict_entries_with_ref_map_and_string_writer(
            &entries,
            options,
            out,
            &map,
            &removed_refs,
            write_string,
        )
    }

    fn unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_string_writer<F>(
        &self,
        out: &mut OutputSink<'_>,
        options: StreamDictionaryOptions,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
        write_string: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    {
        if self.is_reserved() {
            return Err(reserved_unparse_error());
        }
        let entries = stream_dictionary_entries_for_emission(self)?;
        unparse_stream_dict_entries_with_ref_map_and_string_writer(
            &entries,
            options,
            out,
            map,
            removed_refs,
            write_string,
        )
    }

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
    /// [`Self::unparse_object`]/[`Self::unparse_stream_body`] all apply through
    /// `visible_dict_entries`), `/ID` and `/Encrypt` excluded from that
    /// loop and forced last in that order when present, and the closing
    /// `>>` (`:1235`, written unconditionally in both `xref_stream`
    /// cases -- this is why `xref_stream = true` still needs a call into
    /// this function at all, despite skipping the opener). Always
    /// produces the compact (non-QDF) one-line form -- `writeTrailer`'s
    /// own `writeStringQDF` calls (`:1169,1175,1190,1195,1233`) are
    /// QDF-only formatting this primitive does not replicate, matching
    /// handle-native trailer serializer's identical compact-only scope; the
    /// QDF classic trailer is emitted separately by
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
    /// either the legacy raw serializer or this primitive ever runs. This
    /// primitive has no `which`/`size`/`prev`
    /// parameters at all, so `t_lin_first` is out of scope for the same
    /// reason `t_lin_second` is (see below). `/ID` and `/Encrypt` are
    /// read from `self`'s own stored values instead of from writer state
    /// -- the caller is expected to have already placed the correct
    /// values there (`apply_encrypt_trailer_handle_entries` and the canonical
    /// ID helpers in `writer.rs`), the same contract already established by
    /// qpdf's `writeTrailer` is preserved for that dimension.
    ///
    /// `id_writer`, when `Some`, substitutes for the stored `/ID` value
    /// (used by the deterministic-`/ID` writer to emit a content-derived
    /// identifier inline). When `None`, the stored `/ID` value is written
    /// in qpdf's compact `[<hex1><hex2>]` shape with no spaces
    /// (mirroring qpdf's established compact byte shape, implemented here
    /// directly on `ObjectHandle` rather
    /// than bridged through `Object` -- see `write_id_style_value_handle`
    /// below); an indirect `/ID` value writes as its own `"N G R"`
    /// reference form instead, matching `unparse_child`'s reference-vs-recurse
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
    #[cfg(test)]
    fn write_trailer(
        &self,
        out: &mut OutputSink<'_>,
        xref_stream: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
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

    /// Trailer writer with the same live-handle reference remapping used by
    /// the canonical full-rewrite body route.
    ///
    /// `writeTrailer` is a separate qpdf responsibility from
    /// `unparseObject`: it does not apply ordinary dictionary null
    /// suppression, but its child values still have to be emitted from the
    /// live handle graph and rewritten into the output-number space.  The
    /// `qdf` flag selects qpdf's line-oriented classic-trailer spelling and
    /// passes that mode through to direct child containers, as
    /// `writeTrailer`'s `unparseChild(..., 1, 0)` does
    /// (`QPDFWriter.cc:1160-1236`). Indirect child handles remain references in
    /// either mode.
    #[allow(clippy::too_many_arguments)] // qpdf keeps trailer layout, ID, mapping, and visibility controls orthogonal
    fn write_trailer_with_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        xref_stream: bool,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
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
            unparse_trailer_entries_with_ref_map(
                &entries,
                xref_stream,
                qdf,
                id_writer,
                map,
                removed_refs,
                suppress_null_values,
                out,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn write_trailer_with_ref_map_and_kind(
        &self,
        out: &mut OutputSink<'_>,
        kind: TrailerKind,
        xref_stream: bool,
        qdf: bool,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
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
            unparse_trailer_entries_with_ref_map_and_kind(
                &entries,
                kind,
                xref_stream,
                qdf,
                id_writer,
                map,
                removed_refs,
                suppress_null_values,
                None,
                out,
            )
        })
    }

    /// Serialize a synthetic xref-stream dictionary from live handles in
    /// lexicographic key order. Unlike `writeTrailer`, an xref stream owns its
    /// surrounding `<< >>` and therefore does not use the trailer prefix or
    /// `/ID`-last ordering. This is the handle-native counterpart of the
    /// structural dictionary writer in `QPDFWriter.cc:2391-2495`.
    #[cfg(test)]
    fn write_dictionary_with_ref_map_and_id_writer(
        &self,
        out: &mut OutputSink<'_>,
        id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
        suppress_null_values: bool,
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
            unparse_dictionary_entries_with_ref_map_and_id_writer(
                &entries,
                id_writer,
                map,
                removed_refs,
                suppress_null_values,
                out,
            )
        })
    }

    /// Serialize a trailer `/ID` value in qpdf's compact
    /// `[<hex0><hex1>]` form while retaining the live handle and reference-map
    /// boundary. Linearized classic trailers keep their fixed-width `/Prev`
    /// field outside the generic trailer primitive, but still use this helper
    /// for the identifier itself (`QPDFWriter.cc:1194-1222`).
    fn write_id_value_with_ref_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<ObjectRef>,
    ) -> Result<()> {
        let map = qpdf_obj_gen_map_from_object_ref_map(map);
        let removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
        write_id_style_value_handle_with_ref_map(self, out, &map, &removed_refs)
    }

    fn write_id_value_with_qpdf_obj_gen_map(
        &self,
        out: &mut OutputSink<'_>,
        map: &dyn Fn(QpdfObjGen) -> Result<ObjectRef>,
        removed_refs: &BTreeSet<QpdfObjGen>,
    ) -> Result<()> {
        write_id_style_value_handle_with_ref_map(self, out, map, removed_refs)
    }
}

/// Classic-trailer sibling that supplies a live direct Catalog to the root
/// child serializer. This preserves direct stream payload/framing without
/// materializing the complete trailer or PDF body.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_trailer_with_ref_map_and_kind_and_direct_root(
    trailer: &ObjectHandle,
    out: &mut OutputSink<'_>,
    kind: TrailerKind,
    xref_stream: bool,
    qdf: bool,
    id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
    direct_root: &ObjectHandle,
) -> Result<()> {
    if trailer.is_reserved() {
        return Err(reserved_unparse_error());
    }
    trailer.try_dereference()?;
    trailer.with_value(|value| {
        let entries: Vec<(Vec<u8>, ObjectHandle)> = match value {
            Some(ObjectValue::Dictionary(entries)) => entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            _ => Vec::new(), // cov:ignore: qpdf's writer-trailer path always supplies a resolved dictionary handle.
        };
        unparse_trailer_entries_with_ref_map_and_kind(
            &entries,
            kind,
            xref_stream,
            qdf,
            id_writer,
            map,
            removed_refs,
            suppress_null_values,
            Some(direct_root),
            out,
        )
    })
}

/// Apply qpdf's stream-dictionary preparation to a shallow copy.
///
/// `/Length` is always supplied by the writer. qpdf then removes an empty
/// `/DecodeParms` array, removes only `/Filter` and `/DecodeParms` when the
/// stream was successfully filtered, or strips `/Crypt` from the existing
/// filter list when the raw source is retained. External-file keys (`/F`,
/// `/FFilter`, and `/FDecodeParms`) are untouched in every branch
/// (`libqpdf/QPDFWriter.cc:1440-1485`).
fn prepare_stream_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
) -> Result<Vec<(Vec<u8>, ObjectHandle)>> {
    let mut prepared: Vec<_> = entries.to_vec();

    // qpdf probes an empty `/DecodeParms` array before deciding whether the
    // filtered branch will remove the key (`QPDFWriter.cc:1444-1455`). This
    // lookup is intentionally part of the same lazy-resolution boundary.
    if let Some((index, value)) = prepared
        .iter()
        .enumerate()
        .find(|(_, (key, _))| key.as_slice() == b"/DecodeParms")
    {
        if value.1.try_array_len()?.is_some_and(|length| length == 0) {
            prepared.remove(index);
        }
    }

    if options.remove_filter_parameters {
        prepared
            .retain(|(key, _)| key.as_slice() != b"/Filter" && key.as_slice() != b"/DecodeParms");
    } else {
        remove_crypt_filter_from_entries(&mut prepared)?;
    }
    Ok(prepared)
}

/// Return the dictionary values that the stream serializer will expose to
/// `unparseChild` after qpdf's shallow-copy preparation. The returned handles
/// intentionally retain their original identity: in particular, removing
/// `/Crypt` from an indirect filter/decode-parameter array mutates the shared
/// array holder and returns that holder here instead of manufacturing a direct
/// replacement array.
pub(crate) fn prepared_stream_dictionary_children(
    dictionary: &ObjectHandle,
    options: StreamDictionaryOptions,
) -> Result<Vec<ObjectHandle>> {
    let entries = stream_dictionary_entries_for_emission(dictionary)?;
    // Discovery must observe the same post-preparation edges without mutating
    // the dictionary that will be serialized immediately afterwards. qpdf
    // performs both actions against one shallow output copy; in particular,
    // removing `/Crypt` empties the shared filter/parameter arrays while the
    // same pass still emits those now-empty arrays. Isolate only the two
    // mutable array containers for this read-only discovery view and retain
    // their child identities.
    let discovery_entries = entries
        .iter()
        .map(|(key, value)| {
            if matches!(key.as_slice(), b"/Filter" | b"/DecodeParms") {
                value.try_dereference()?;
                if let Some(items) = value.try_as_array()? {
                    return Ok((key.clone(), ObjectHandle::array(items)));
                }
            }
            Ok((key.clone(), value.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    let prepared = prepare_stream_dict_entries(&discovery_entries, options)?;
    Ok(visible_dict_entries(&prepared)?
        .into_iter()
        .filter(|(key, _)| key.as_slice() != b"/Length")
        .map(|(key, value)| {
            // Keep an indirect filter/parameter holder's source identity even
            // though the isolated discovery copy supplied the preparation
            // decision. The later serializer mutates the original shallow
            // holder exactly once and must resolve it through the queue map.
            if matches!(key.as_slice(), b"/Filter" | b"/DecodeParms") {
                entries
                    .iter()
                    .find(|(original_key, _)| original_key.as_slice() == key.as_slice())
                    .map(|(_, original_value)| original_value.clone())
                    .unwrap_or_else(|| value.clone())
            } else {
                value.clone()
            }
        })
        .collect())
}

/// Remove the first `/Crypt` filter and its paired decode parameters from a
/// copied dictionary. This mutates only the copy, matching qpdf's shallow
/// `unparseObject` preparation.
fn remove_crypt_filter_from_entries(entries: &mut Vec<(Vec<u8>, ObjectHandle)>) -> Result<()> {
    let Some(filter_index) = entries
        .iter()
        .position(|(key, _)| key.as_slice() == b"/Filter")
    else {
        return Ok(());
    };
    let filter = entries[filter_index].1.clone();
    if filter.try_is_name_and_equals(b"Crypt")? {
        entries.remove(filter_index);
        entries.retain(|(key, _)| key.as_slice() != b"/DecodeParms");
        return Ok(());
    }
    let Some(filters) = filter.try_as_array()? else {
        return Ok(());
    };
    let mut crypt_index = None;
    for (index, item) in filters.iter().enumerate() {
        if item.try_is_name_and_equals(b"Crypt")? {
            crypt_index = Some(index);
            break;
        }
    }
    let Some(crypt_index) = crypt_index else {
        return Ok(());
    };
    // qpdf's shallow dictionary copy still shares this array QObject with
    // the source dictionary; `eraseItem` therefore mutates the live alias in
    // place. Keep that identity by calling the canonical array mutator
    // instead of replacing the value with a newly allocated array.
    filter.erase_array_item(crypt_index)?;

    let Some(decode_index) = entries
        .iter()
        .position(|(key, _)| key.as_slice() == b"/DecodeParms")
    else {
        // `getKey` returns qpdf's contextual null when the key is absent, and
        // qpdf still calls `eraseItem` on that handle. Preserve that lookup's
        // warning/error boundary instead of silently skipping the call.
        // qpdf asks the shallow output dictionary for `/DecodeParms` after an
        // empty value has already been removed. Construct that same direct
        // output dictionary here; its missing-key null retains the qpdf
        // description but no owning-document context, so `eraseItem` keeps
        // the warning/exception boundary rather than treating the old source
        // array as still present.
        let output_dictionary = ObjectHandle::dictionary(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        let decode = output_dictionary.try_get_key(b"/DecodeParms")?;
        decode.erase_array_item(crypt_index)?;
        return Ok(()); // cov:ignore: qpdf's contextless missing-key erase always raises; this success continuation is defensive only
    };
    let decode = entries[decode_index].1.clone();
    // qpdf calls `decode_parms.eraseItem(idx)` unconditionally after removing
    // the Crypt filter. The canonical mutator preserves its warning and
    // out-of-bounds behavior for malformed/non-array parameter values.
    decode.erase_array_item(crypt_index)?;
    Ok(())
}

type StreamDictionaryEntries = Vec<(Vec<u8>, ObjectHandle)>;

/// Snapshot a stream dictionary's entries after resolving the same nested
/// dictionary handle qpdf's stream writer receives. The emission primitives
/// build their own direct shallow-copy view when a removed key must be looked
/// up again, so they do not accidentally inspect the live source dictionary.
fn stream_dictionary_entries_for_emission(
    handle: &ObjectHandle,
) -> Result<StreamDictionaryEntries> {
    handle.try_dereference()?;
    if let Some(entries) = handle.with_value(|value| match value {
        Some(ObjectValue::Dictionary(entries)) => Some(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        _ => None,
    }) {
        return Ok(entries);
    }

    let Some(stream_dict) = handle.with_value(|value| match value {
        Some(ObjectValue::Stream(stream)) => Some(stream.stream_dict.clone()),
        _ => None,
    }) else {
        return Ok(Vec::new());
    };
    stream_dict.try_dereference()?;
    let entries = stream_dict.with_value(|value| match value {
        Some(ObjectValue::Dictionary(entries)) => entries
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        _ => Vec::new(),
    });
    Ok(entries)
}

fn dictionary_entry_handle(dictionary: &ObjectHandle, key: &[u8]) -> Option<ObjectHandle> {
    dictionary.with_value(|value| match value {
        Some(ObjectValue::Dictionary(entries)) => entries.get(key).cloned(),
        _ => None,
    })
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
// irrelevant to the refiltered output. The corresponding qpdf writer path
// removes these entries before checking which dictionary values are visible.
fn unparse_stream_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child(value, out)?;
        }
    }
    if let Some(length) = length_value {
        out.write_bytes(b" /Length ")?;
        unparse_child(length, out)?;
    }
    if options.add_flate_filter {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")?;
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
#[cfg(test)]
fn unparse_stream_dict_entries_qdf(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    let entries = prepare_stream_dict_entries(entries, StreamDictionaryOptions::preserve())?;
    out.write_bytes(b"<<\n")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_qdf(value, indent + 2, out)?;
        }
        out.write_bytes(b"\n")?;
    }
    if let Some(length) = length_value {
        push_spaces(out, indent + 2)?;
        out.write_bytes(b"/Length ")?;
        unparse_child_qdf(length, indent + 2, out)?;
        out.write_bytes(b"\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn unparse_stream_dict_entries_qdf_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    length_ref: Option<ObjectRef>,
    options: StreamDictionaryOptions,
) -> Result<()> {
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<\n")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_qdf_with_ref_map(value, indent + 2, out, map, removed_refs)?;
        }
        out.write_bytes(b"\n")?;
    }
    if let Some(length_ref) = length_ref {
        push_spaces(out, indent + 2)?;
        out.write_bytes(b"/Length ")?;
        write_object_ref(out, length_ref)?;
        out.write_bytes(b"\n")?;
    } else if let Some(length) = length_value {
        push_spaces(out, indent + 2)?;
        out.write_bytes(b"/Length ")?;
        unparse_child_qdf_with_ref_map(length, indent + 2, out, map, removed_refs)?;
        out.write_bytes(b"\n")?;
    }
    if options.add_flate_filter {
        push_spaces(out, indent + 2)?;
        out.write_bytes(b"/Filter /FlateDecode\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn unparse_stream_dict_entries_qdf_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    length_ref: Option<ObjectRef>,
    options: StreamDictionaryOptions,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<\n")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            out.write_bytes(b"\n")?;
            continue;
        }
        unparse_child_qdf_with_ref_map_and_string_writer(
            value,
            indent + 2,
            out,
            map,
            removed_refs,
            write_string,
        )?; // cov:ignore: LLVM maps the covered mapped stream child call continuation to this line
        out.write_bytes(b"\n")?;
    }
    push_spaces(out, indent + 2)?;
    out.write_bytes(b"/Length ")?;
    if let Some(length_ref) = length_ref {
        write_object_ref(out, length_ref)?;
    } else if let Some(length) = length_value {
        unparse_child_qdf_with_ref_map_and_string_writer(
            length,
            indent + 2,
            out,
            map,
            removed_refs,
            write_string,
        )?; // cov:ignore: LLVM maps the covered mapped length child call continuation to this line
    } else {
        out.write_bytes(b"null")?;
    }
    out.write_bytes(b"\n")?;
    if options.add_flate_filter {
        push_spaces(out, indent + 2)?;
        out.write_bytes(b"/Filter /FlateDecode\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

#[cfg(test)]
fn unparse_stream_dict_entries_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    refiltered: bool,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    let entries = prepare_stream_dict_entries(
        entries,
        StreamDictionaryOptions::from_refiltered(refiltered),
    )?; // cov:ignore: legacy test-only string-writer wrapper delegates to the covered options primitive
    out.write_bytes(b"<<")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            continue;
        }
        unparse_child_with_string_writer(value, out, write_string)?;
    }
    if let Some(length) = length_value {
        out.write_bytes(b" /Length ")?;
        unparse_child_with_string_writer(length, out, write_string)?;
    }
    if refiltered {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

#[cfg(test)]
fn unparse_stream_dict_entries_qdf_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    out.write_bytes(b"<<\n")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(entries)? {
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            out.write_bytes(b"\n")?;
            continue;
        }
        unparse_child_qdf_with_string_writer(value, indent + 2, out, write_string)?;
        out.write_bytes(b"\n")?;
    }
    if let Some(length) = length_value {
        push_spaces(out, indent + 2)?;
        out.write_bytes(b"/Length ")?;
        unparse_child_qdf_with_string_writer(length, indent + 2, out, write_string)?;
        out.write_bytes(b"\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

fn unparse_stream_dict_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    unparse_stream_dict_entries_with_ref_map_and_length(
        entries,
        options,
        out,
        map,
        removed_refs,
        None,
    )
}

fn unparse_stream_dict_entries_with_ref_map_and_length(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    length_override: Option<usize>,
) -> Result<()> {
    // qpdf removes the source `/Length` from its shallow dictionary before any
    // value visibility checks (`QPDFWriter.cc:1440-1442`). In particular, a
    // caller-provided output length must not force-resolve a dangling source
    // length while the snapshot fallback is being prepared.
    let entries_without_length;
    let entries = if length_override.is_some() {
        entries_without_length = entries
            .iter()
            .filter(|(key, _)| key.as_slice() != b"/Length")
            .cloned()
            .collect::<Vec<_>>();
        entries_without_length.as_slice()
    } else {
        entries
    };
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_with_ref_map(value, out, map, removed_refs)?;
        }
    }
    if let Some(length) = length_override {
        out.write_bytes(b" /Length ")?;
        write_decimal_u64(out, length as u64)?;
    } else if let Some(length) = length_value {
        out.write_bytes(b" /Length ")?;
        unparse_child_with_ref_map(length, out, map, removed_refs)?;
    }
    if options.add_flate_filter {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

/// Emit a direct stream dictionary through the qpdf-shaped live key cursor.
///
/// The linearized writer already owns the final payload length, so qpdf's
/// shallow stream-dictionary copy only needs to affect the emitted view: the
/// source `/Length` is discarded, filter parameters may be omitted, and the
/// computed length is written last. Ordinary array-valued filters and `/Crypt`
/// retain the existing snapshot path because their preparation mutates shared
/// child arrays and must preserve that aliasing/error boundary.
fn unparse_stream_dictionary_live_with_qpdf_obj_gen_map_and_removed_and_length(
    dictionary: &ObjectHandle,
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    length: usize,
) -> Result<()> {
    let skip_empty_decode_parms = match dictionary_entry_handle(dictionary, b"/DecodeParms") {
        Some(value) => value.try_array_len()?.is_some_and(|value| value == 0),
        None => false,
    };

    if !options.remove_filter_parameters {
        let has_snapshot_sensitive_filter =
            if let Some(filter) = dictionary_entry_handle(dictionary, b"/Filter") {
                filter.try_dereference()?;
                filter.with_value(|value| matches!(value, Some(ObjectValue::Array(_))))
                    || filter.try_is_name_and_equals(b"Crypt")?
            } else {
                false
            };
        if has_snapshot_sensitive_filter {
            let entries = stream_dictionary_entries_for_emission(dictionary)?;
            return unparse_stream_dict_entries_with_ref_map_and_length(
                &entries,
                options,
                out,
                map,
                removed_refs,
                Some(length),
            );
        }
    }

    out.write_bytes(b"<<")?;
    let mut current_key = LiveDictionaryKeyBuffer::default();
    let mut next_key = LiveDictionaryKeyBuffer::default();
    let mut first_entry = true;
    while let Some(value) = dictionary.next_dictionary_entry_for_live_walk(
        (!first_entry).then_some(current_key.as_slice()),
        &mut next_key,
    ) {
        std::mem::swap(&mut current_key, &mut next_key);
        let key = current_key.as_slice();

        // qpdf removes these keys from its shallow copy before entering the
        // null-suppression loop (`QPDFWriter.cc:1440-1455`).
        if key == b"/Length"
            || (options.remove_filter_parameters && matches!(key, b"/Filter" | b"/DecodeParms"))
            || (key == b"/DecodeParms" && skip_empty_decode_parms)
        {
            first_entry = false;
            continue;
        }

        // qpdf checks `isNull()` before the rewrite-specific removed set can
        // suppress an indirect child. Keep that order at this live boundary.
        if value.try_is_null()?
            || value
                .qpdf_obj_gen()
                .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            first_entry = false;
            continue;
        }

        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        if key == b"/Contents" {
            let force_hex_contents = dict_is_sig_with_byte_range_handle(dictionary)?;
            if try_write_sig_contents_hex_string(&value, force_hex_contents, out)? {
                first_entry = false;
                continue;
            }
        }
        unparse_child_with_ref_map(&value, out, map, removed_refs)?;
        first_entry = false;
    }

    out.write_bytes(b" /Length ")?;
    write_decimal_u64(out, length as u64)?;
    if options.add_flate_filter {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

fn unparse_stream_dict_entries_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            continue;
        }
        unparse_child_with_ref_map_and_string_writer(value, out, map, removed_refs, write_string)?;
    }
    if let Some(length) = length_value {
        out.write_bytes(b" /Length ")?;
        unparse_child_with_ref_map_and_string_writer(length, out, map, removed_refs, write_string)?;
    }
    if options.add_flate_filter {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

// Writes one child handle's bytes for the plain-unparse family serviced by
// `unparse_object_walk` below: an indirect child always writes as its own
// `"N G R"` reference form, never recursed into — the same reference-vs-
// recurse split used by the child-unparse helpers above already
// apply, mirroring `QPDFWriter::unparseChild`'s own `child.isIndirect()`
// check (`libqpdf/QPDFWriter.cc:1144-1156`, the check itself at `:1149`),
// which `unparseObject`'s array-element and dictionary-value loops call into
// for exactly this decision (`:1342`, `:1503`) instead of inlining it. A
// direct child recurses through `unparse_object_walk`.
//
// No separate reserved check here, for the same reason the child-unparse helper
// has none in its own reference-vs-recurse decision (see its own doc): the
// decision below is `isIndirect()`-only, matching `unparseChild` exactly,
// and never inspects the referenced object's resolved type. An *indirect*
// reserved child always takes the reference-token branch below without ever
// being dereferenced here. A *direct* reserved child does still get
// rejected, but one level down: the `None` branch recurses into
// `unparse_object_walk`, whose own `is_reserved` check
// (`QPDF_Reserved::unparse()`, `libqpdf/QPDF_Reserved.cc:22-26`'s throw)
// runs on whatever handle it is entered with, top-level `self` or a
// recursed-into direct child alike -- this function does not need its own
// copy of that check to get the same result.
pub(crate) fn unparse_child(handle: &ObjectHandle, out: &mut OutputSink<'_>) -> Result<()> {
    // `QPDFWriter::unparseChild` branches on `isIndirect()`
    // (`libqpdf/QPDFWriter.cc:1148`), which is `obj != 0`
    // (`include/qpdf/QPDFObjGen.hh:78-81`) and never inspects the generation.
    // Read the raw identity so a generation outside the `N G R` projection
    // range still emits a reference instead of being inlined.
    if let Some(object_gen) = handle
        .qpdf_obj_gen()
        .filter(|object_gen| object_gen.is_indirect())
    {
        write_qpdf_object_gen(out, object_gen)?;
        return Ok(());
    }
    if !handle.is_initialized() {
        handle.try_dereference()?;
    }
    if write_direct_child(handle, out)? {
        return Ok(());
    }
    unparse_object_walk(handle, out)
}

/// Serialize a direct scalar child without entering the recursive container
/// walker. Container values return `false` so the existing walker retains
/// ownership of their descendant traversal.
fn write_direct_child(handle: &ObjectHandle, out: &mut OutputSink<'_>) -> Result<bool> {
    handle.with_value(|value| match value {
        Some(ObjectValue::Array(_) | ObjectValue::Dictionary(_) | ObjectValue::Stream(_)) => {
            Ok(false)
        }
        Some(value) => {
            unparse_object_value(value, out)?;
            Ok(true)
        }
        None => {
            // cov:ignore-start: a successful dereference exposes a value; keep
            // the same defensive null spelling as the recursive walker.
            out.write_bytes(b"null")?;
            Ok(true)
            // cov:ignore-end
        }
    })
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
pub(crate) fn visible_dict_entries(
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
}

/// Copy the root container and reconcile `/ADBE` with qpdf's child aliasing.
fn root_output_copy_with_adbe(
    source: &ObjectHandle,
    final_pdf_version: &str,
    final_extension_level: i64,
    apply_adbe_reconciliation: bool,
) -> Result<ObjectHandle> {
    let root = source.unsafe_shallow_copy()?;
    // qpdf's special /Extensions handling is guarded by
    // `old_og == m->root_og` (`QPDFWriter.cc:1374`). The live writer passes
    // false for a direct Catalog, whose unparse call has no root object
    // identity, and true for the actual indirect Catalog. Keep this context
    // explicit instead of inferring it from the handle: low-level writer
    // tests also use synthetic direct dictionaries that exercise the
    // reconciliation primitive without representing a document root.
    if !apply_adbe_reconciliation {
        return Ok(root);
    }
    let mut extensions = if root.try_has_key(b"/Extensions")?
        && root.try_get_key(b"/Extensions")?.try_is_dictionary()?
    {
        Some(root.try_get_key(b"/Extensions")?)
    } else {
        None
    };
    let (have_adbe, have_other) = if let Some(extensions) = &extensions {
        let mut keys = extensions.try_get_keys()?;
        let have_adbe = keys.remove(b"/ADBE".as_slice());
        (have_adbe, !keys.is_empty())
    } else {
        (false, false)
    };
    let need_adbe = final_extension_level > 0;
    if need_adbe {
        if !(have_other || have_adbe) {
            let created = ObjectHandle::dictionary(Vec::new());
            root.replace_key(b"/Extensions", created.clone())?;
            extensions = Some(created);
        }
    } else if !have_other && have_adbe {
        root.remove_key(b"/Extensions");
        extensions = None;
    }

    if let Some(extensions) = extensions {
        let adbe = extensions.try_get_key(b"/ADBE")?;
        let preserves_existing = adbe.try_is_dictionary()?
            && adbe
                .try_get_key(b"/BaseVersion")?
                .try_is_name_and_equals(final_pdf_version.as_bytes())?
            && adbe.try_get_key(b"/ExtensionLevel")?.try_as_integer()?
                == Some(final_extension_level);
        if !preserves_existing {
            if need_adbe {
                let replacement = ObjectHandle::dictionary(vec![
                    (
                        b"/BaseVersion".to_vec(),
                        ObjectHandle::name(final_pdf_version.as_bytes().to_vec()),
                    ),
                    (
                        b"/ExtensionLevel".to_vec(),
                        ObjectHandle::integer(final_extension_level),
                    ),
                ]);
                extensions.replace_key(b"/ADBE", replacement)?;
            } else {
                extensions.remove_key(b"/ADBE");
            }
        }
    }

    Ok(root)
}

// The sole recursion hub for the plain unparse family (`ObjectHandle::
// unparse_object` and its callees below), mirroring the unparse walk's
// own single-hub pattern above for the same stack-growth reason: an
// `ObjectHandle` tree built through public factories carries no depth bound
// the parser enforces on parsed input. Also forces resolution of `handle`
// itself before inspecting its value: every call into this hub either comes
// from `unparse_object`'s top-level entry point (whose argument may still be
// an unresolved indirect handle) or from a direct child that `unparse_child`
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
        ObjectValue::Stream(stream) => Some(UnparseContainer::Stream(stream.stream_dict.clone())),
        _ => None,
    }
}

fn is_direct_scalar_value(value: &ObjectValue) -> bool {
    matches!(
        value,
        ObjectValue::Null
            | ObjectValue::Boolean(_)
            | ObjectValue::Integer(_)
            | ObjectValue::Real(_)
            | ObjectValue::RealLiteral { .. }
            | ObjectValue::Name(_)
            | ObjectValue::String(_)
            | ObjectValue::Operator(_)
            | ObjectValue::InlineImage(_)
    )
}

fn is_direct_scalar_handle(handle: &ObjectHandle) -> bool {
    // `is_direct()` is not the predicate this fast path needs. qpdf treats
    // object number 0 as non-indirect, so a handle carrying a raw `0 G`
    // identity answers `is_direct() == true` while
    // `unparse_child_with_dynamic_ref_map_and_string_writer` emits `null` for
    // it (`!object_gen.is_indirect()`). Require the absence of any raw
    // identity so such a child keeps taking the slow path and the two routes
    // agree byte for byte.
    handle.qpdf_obj_gen().is_none()
        && handle.with_value(|value| value.is_some_and(is_direct_scalar_value))
}

fn write_direct_scalar_with_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    handle.with_value(|value| match value {
        Some(ObjectValue::String(bytes)) => write_string(out, bytes),
        Some(value) => unparse_object_value(value, out),
        None => out.write_bytes(b"null"), // cov:ignore: is_direct_scalar_handle requires a concrete direct value before this helper runs.
    })
}

fn try_write_direct_scalar_container_with_string_writer<F>(
    value: &ObjectValue,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<bool>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    match value {
        ObjectValue::Array(children) if children.iter().all(is_direct_scalar_handle) => {
            out.write_bytes(b"[")?;
            for child in children {
                out.write_bytes(b" ")?;
                write_direct_scalar_with_string_writer(child, out, write_string)?;
            }
            out.write_bytes(b" ]")?;
            Ok(true)
        }
        ObjectValue::Dictionary(entries)
            if entries
                .iter()
                .all(|(_, child)| is_direct_scalar_handle(child)) =>
        {
            let is_signature = entries
                .iter()
                .find(|(key, _)| key.as_slice() == b"/Type")
                .is_some_and(|(_, value)| value.as_name().as_deref() == Some(b"Sig"));
            let has_byte_range = is_signature
                && entries
                    .iter()
                    .find(|(key, _)| key.as_slice() == b"/ByteRange")
                    .is_some_and(|(_, value)| !value.is_null());
            let force_hex_contents = is_signature && has_byte_range;

            out.write_bytes(b"<<")?;
            for (key, child) in entries {
                if child.is_null() {
                    continue;
                }
                out.write_bytes(b" ")?;
                write_dictionary_key(out, key)?;
                out.write_bytes(b" ")?;
                if force_hex_contents
                    && key.as_slice() == b"/Contents"
                    && child.with_value(|value| -> Result<bool> {
                        if let Some(ObjectValue::String(bytes)) = value {
                            crate::pdf_syntax::write_hex_string(out, bytes)?;
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    })?
                {
                    continue;
                }
                write_direct_scalar_with_string_writer(child, out, write_string)?;
            }
            out.write_bytes(b" >>")?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn unparse_container(container: UnparseContainer, out: &mut OutputSink<'_>) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            out.write_bytes(b"[")?;
            for child in children {
                out.write_bytes(b" ")?;
                unparse_child(&child, out)?;
            }
            out.write_bytes(b" ]")?;
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

fn unparse_object_walk(handle: &ObjectHandle, out: &mut OutputSink<'_>) -> Result<()> {
    unparse_object_walk_hub(|| {
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
                out.write_bytes(b"null")?;
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

pub(crate) fn unparse_object_value(value: &ObjectValue, out: &mut OutputSink<'_>) -> Result<()> {
    match value {
        ObjectValue::Null => out.write_bytes(b"null")?,
        ObjectValue::Unresolved => return Err(unresolved_unparse_error()),
        ObjectValue::Reserved => return Err(reserved_unparse_error()),
        ObjectValue::Destroyed => return Err(destroyed_unparse_error()),
        ObjectValue::Boolean(v) => out.write_bytes(if *v { b"true" } else { b"false" })?,
        ObjectValue::Integer(v) => write_decimal_i64(out, *v)?,
        ObjectValue::Real(v) => out.write_bytes(v.to_string().as_bytes())?,
        ObjectValue::RealLiteral { value, literal } => {
            if crate::pdf_syntax::real_literal_is_safe(literal, *value) {
                out.write_bytes(literal)?;
            } else {
                out.write_bytes(value.to_string().as_bytes())?;
            }
        }
        ObjectValue::Name(name) => {
            out.write_bytes(b"/")?;
            crate::pdf_syntax::write_name_escaped(out, name)?;
        }
        ObjectValue::String(value) => crate::pdf_syntax::write_string_value(out, value)?,
        ObjectValue::Operator(value) | ObjectValue::InlineImage(value) => {
            out.write_bytes(value)?;
        }
        ObjectValue::Array(children) => {
            // QPDFWriter.cc:1334-1345: no token-boundary rule, a space is
            // written before every element regardless of adjacency.
            // cov:ignore-start: unparse_object_walk snapshots every array into UnparseContainer before this scalar fallback can dispatch.
            out.write_bytes(b"[")?;
            for child in children {
                out.write_bytes(b" ")?;
                unparse_child(child, out)?;
            }
            out.write_bytes(b" ]")?;
            // cov:ignore-end
        }
        ObjectValue::Dictionary(entries) => {
            // cov:ignore-start: the recursion hub snapshots every container before this fallback is entered.
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries(&entries, out)?;
        }
        ObjectValue::Stream(stream) => {
            // Reachable two ways, not just one: a *direct* Stream value (no
            // qpdf counterpart -- a real QPDFObjectHandle's resolved value
            // is never itself a stream outside an indirect object), and an
            // *indirect* `self` at the top level of `unparse_object` that
            // resolves to a stream (a real, reachable qpdf shape -- see
            // `ObjectHandle::unparse_object`'s own doc). The latter is
            // reachable here because `unparse_object`/`unparse_object_walk`
            // call this dispatch directly on `self`, bypassing `unparse_child`
            // entirely; `unparse_child` only gates *child* positions (array
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
            // scoped responsibility -- this generic dispatch does not
            // implement qpdf's real
            // stream-writing path for the indirect case either.
            unparse_object_walk(&stream.stream_dict, out)?;
            // cov:ignore-end
        }
    }
    Ok(())
}

type QpdfObjGenMap<'a> = dyn Fn(QpdfObjGen) -> Result<ObjectRef> + 'a;

pub(crate) fn qpdf_obj_gen_map_from_object_ref_map<'a>(
    map: &'a dyn Fn(ObjectRef) -> Result<ObjectRef>,
) -> impl Fn(QpdfObjGen) -> Result<ObjectRef> + 'a {
    move |object_gen| {
        let object_ref = object_gen.to_object_ref().ok_or_else(|| {
            Error::Unsupported(format!(
                "qpdf raw object identity {} {} cannot be used by an ObjectRef map",
                object_gen.get_obj(),
                object_gen.get_gen()
            ))
        })?;
        map(object_ref)
    }
}

pub(crate) fn qpdf_obj_gen_set_from_object_ref_set(
    object_refs: &BTreeSet<ObjectRef>,
) -> Result<BTreeSet<QpdfObjGen>> {
    object_refs
        .iter()
        .copied()
        .map(QpdfObjGen::try_from_object_ref)
        .collect()
}

// Ref-map sibling of `unparse_child` above -- same reference-vs-recurse split
// on `handle.object_ref()` alone, so the same reasoning applies: an
// *indirect* reserved child takes this `Some` branch (writing its mapped
// reference token, or `null` if renumbering removed it, per the
// qpdf-rewrite null-handling below) without ever being dereferenced here.
// See `unparse_child`'s own doc for why no separate reserved check belongs in
// a child-position function at all: the `None` branch below recurses into
// `unparse_object_walk_with_ref_map`, whose own `is_reserved` check already
// rejects a *direct* reserved child the same way it rejects a reserved
// top-level `self`. This is the primitive `writer/plain/body.rs`/
// `writer/plain/plan.rs` actually call in production, so a direct reserved child
// reaching a live document write is already covered by this path.
//
// `pub(crate)` so `linearization::writer::canonical_linearization_trailer_entries`
// can share this exact reference-vs-direct-value dispatch for its own
// non-structural trailer values (D14: one child-value serializer instead of
// linearization retyping the indirect-reference branch independently).
pub(crate) fn unparse_child_with_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect() || removed_refs.contains(&object_gen) {
            // qpdf's direct-null identity is object number zero, not an
            // output reference (QPDFObjectHandle.cc:344-350). A removed
            // identity follows the same null path in the qpdf rewrite.
            out.write_bytes(b"null")?;
            return Ok(());
        }
        let mapped = map(object_gen)?;
        write_object_ref(out, mapped)?;
        return Ok(());
    }
    if !handle.is_initialized() {
        handle.try_dereference()?;
    }
    if write_direct_child_with_ref_map(handle, out, map, removed_refs)? {
        return Ok(());
    }
    unparse_object_walk_with_ref_map(handle, out, map, removed_refs)
}

fn write_direct_child_with_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<bool> {
    handle.with_value(|value| match value {
        Some(ObjectValue::Array(_) | ObjectValue::Dictionary(_) | ObjectValue::Stream(_)) => {
            Ok(false)
        }
        Some(value) => {
            unparse_object_value_with_ref_map(value, out, map, removed_refs)?;
            Ok(true)
        }
        None => {
            // cov:ignore-start: a successful dereference exposes a value; keep
            // the same defensive null spelling as the recursive walker.
            out.write_bytes(b"null")?;
            Ok(true)
            // cov:ignore-end
        }
    })
}

fn unparse_object_walk_with_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| -> Result<RefMapContainer> {
            match value {
                Some(ObjectValue::Array(_)) => Ok(RefMapContainer::Array),
                Some(ObjectValue::Dictionary(_)) => Ok(RefMapContainer::Dictionary),
                Some(ObjectValue::Stream(stream)) => {
                    Ok(RefMapContainer::Stream(stream.stream_dict.clone()))
                }
                Some(value) => {
                    // Scalar payloads are written under the borrow. Container
                    // edges are walked after the borrow is released, matching
                    // qpdf's per-child `unparseChild` boundary.
                    unparse_object_value_with_ref_map(value, out, map, removed_refs)?;
                    Ok(RefMapContainer::Written)
                }
                None => {
                    // cov:ignore-start: successful dereference exposes Null for
                    // the null fallback or errors while unresolved.
                    out.write_bytes(b"null")?;
                    Ok(RefMapContainer::Written)
                    // cov:ignore-end
                }
            }
        })?;
        match container {
            RefMapContainer::Array => unparse_array_with_ref_map(handle, out, map, removed_refs),
            RefMapContainer::Dictionary => {
                unparse_dictionary_with_ref_map(handle, out, map, removed_refs)
            }
            RefMapContainer::Stream(stream_dict) => {
                unparse_object_walk_with_ref_map(&stream_dict, out, map, removed_refs)
            }
            RefMapContainer::Written => Ok(()),
        }
    })
}

enum RefMapContainer {
    Array,
    Dictionary,
    Stream(ObjectHandle),
    Written,
}

fn unparse_array_with_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    out.write_bytes(b"[")?;
    let items = handle.try_array_items()?;
    let mut cursor = items.begin();
    while !cursor.is_end() {
        out.write_bytes(b" ")?;
        let child = cursor.current();
        unparse_child_with_ref_map(&child, out, map, removed_refs)?;
        cursor.next();
    }
    out.write_bytes(b" ]")?;
    Ok(())
}

fn unparse_dictionary_with_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    out.write_bytes(b"<<")?;
    let mut current_key = LiveDictionaryKeyBuffer::default();
    let mut next_key = LiveDictionaryKeyBuffer::default();
    let mut first_entry = true;
    while let Some(value) = handle.next_dictionary_entry_for_live_walk(
        (!first_entry).then_some(current_key.as_slice()),
        &mut next_key,
    ) {
        std::mem::swap(&mut current_key, &mut next_key);
        // qpdf's dictionary writer checks isNull() at the current key before
        // calling unparseChild. Preserve that resolution boundary even when
        // the rewrite-specific removed set will discard the edge afterwards.
        let is_null = value.try_is_null()?;
        if is_null
            || value
                .qpdf_obj_gen()
                .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            first_entry = false;
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, current_key.as_slice())?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            current_key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range_handle(handle)?;
        if !try_write_sig_contents_hex_string(&value, force_hex_string, out)? {
            unparse_child_with_ref_map(&value, out, map, removed_refs)?;
        }
        first_entry = false;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

fn unparse_object_value_with_ref_map(
    value: &ObjectValue,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    match value {
        ObjectValue::Array(children) => {
            // cov:ignore-start: unparse_object_walk_with_ref_map snapshots every array before this value fallback is entered.
            out.write_bytes(b"[")?;
            for child in children {
                out.write_bytes(b" ")?;
                unparse_child_with_ref_map(child, out, map, removed_refs)?;
            }
            out.write_bytes(b" ]")?;
            // cov:ignore-end
        }
        ObjectValue::Dictionary(entries) => {
            // cov:ignore-start: the ref-map recursion hub snapshots every dictionary before this fallback is entered.
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries_with_ref_map(&entries, out, map, removed_refs)?;
        }
        ObjectValue::Stream(stream) => {
            unparse_object_walk_with_ref_map(&stream.stream_dict, out, map, removed_refs)?;
            // cov:ignore-end
        }
        _ => unparse_object_value(value, out)?,
    }
    Ok(())
}

fn unparse_dict_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    out.write_bytes(b"<<")?;
    for (key, value) in visible_dict_entries(entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_with_ref_map(value, out, map, removed_refs)?; // cov:ignore: this function's only caller (unparse_object_value_with_ref_map's dictionary arm) is itself unreachable in production -- the ref-map recursion hub always snapshots and walks a dictionary's entries before that fallback is entered.
        }
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

pub(crate) type DynamicObjectRefMap<'a> = dyn FnMut(&ObjectHandle) -> Result<ObjectRef> + 'a;

/// Writer-owned hook for a direct stream encountered inside an array or
/// dictionary. qpdf emits the stream payload inline at this child boundary;
/// stream filtering and encryption remain the responsibility of the current
/// writer consumer.
pub(crate) trait DynamicDirectStreamWriter {
    #[allow(clippy::type_complexity)]
    fn write_direct_stream(
        &mut self,
        stream: &ObjectHandle,
        out: &mut OutputSink<'_>,
        map: &mut DynamicObjectRefMap<'_>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut dyn FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    ) -> Result<()>;
}

/// Default raw-preserve direct-stream policy for low-level dynamic object
/// emission. Full writer routes replace this with their stream filter/encrypt
/// policy at the same qpdf `unparseChild` boundary.
pub(crate) struct DefaultDynamicDirectStreamWriter {
    pub(crate) newline_before_endstream: Option<crate::writer::NewlineBeforeEndstream>,
    pub(crate) qdf_mode: bool,
}

impl DynamicDirectStreamWriter for DefaultDynamicDirectStreamWriter {
    fn write_direct_stream(
        &mut self,
        stream: &ObjectHandle,
        out: &mut OutputSink<'_>,
        map: &mut DynamicObjectRefMap<'_>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut dyn FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
    ) -> Result<()> {
        stream.try_dereference()?;
        let dict = stream.as_stream_dict().ok_or_else(|| {
            // cov:ignore-start: dynamic child dispatch calls this hook only after a stream shape probe.
            Error::Internal("direct stream disappeared during emission".to_string())
            // cov:ignore-end
        })?; // cov:ignore: the dynamic direct-stream hook is called only after the stream shape probe.
        let data = stream.get_raw_stream_data()?;
        let dict = dict.unsafe_shallow_copy()?;
        dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(data.len()).map_err(|_| {
                // cov:ignore-start: an allocatable direct stream payload fits in i64.
                Error::Unsupported("direct stream /Length does not fit in i64".to_string())
                // cov:ignore-end
            })?), // cov:ignore: allocatable direct stream lengths fit the i64 PDF length domain.
        )?; // cov:ignore: LLVM attributes the successful direct-stream dictionary replacement continuation separately.
        let entries = stream_dictionary_entries_for_emission(&dict)?;
        unparse_stream_dict_entries_with_dynamic_ref_map_and_string_writer(
            &entries,
            StreamDictionaryOptions::preserve(),
            out,
            map,
            removed_refs,
            write_string,
            self,
        )?; // cov:ignore: the dynamic direct-stream dictionary serializer is exercised by the direct-stream policy regression.
        out.write_bytes(b"\nstream\n")?;
        out.write_bytes(data.as_ref())?;
        if self.qdf_mode {
            if crate::writer::serialize::framing_adds_newline_with_qdf(
                data.as_ref(),
                self.newline_before_endstream
                    .unwrap_or(crate::writer::NewlineBeforeEndstream::Never),
                true,
            ) {
                out.write_bytes(b"\n")?;
            } // cov:ignore: LLVM attributes the covered QDF direct-stream newline branch exit to the newline write.
        } else if self
            .newline_before_endstream
            .is_some_and(|policy| matches!(policy, crate::writer::NewlineBeforeEndstream::Yes))
        {
            out.write_bytes(b"\n")?;
        }
        out.write_bytes(b"endstream")
    }
}

/// Dynamic object emission with an injected direct-stream policy. This keeps
/// live reference discovery and stream framing on the writer-owned boundary
/// without rebuilding the complete PDF in a `Vec`.
#[allow(clippy::type_complexity)]
pub(crate) fn unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
    direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    unparse_object_walk_with_dynamic_ref_map_and_string_writer(
        handle,
        out,
        map,
        removed_refs,
        write_string,
        direct_stream_writer,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn unparse_root_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer<
    F,
>(
    source: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    final_pdf_version: &str,
    final_extension_level: i64,
    apply_adbe_reconciliation: bool,
    write_string: &mut F,
    direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    let root = root_output_copy_with_adbe(
        source,
        final_pdf_version,
        final_extension_level,
        apply_adbe_reconciliation,
    )?; // cov:ignore: the root-copy success continuation is covered by the direct-root writer differential.
    unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
        &root,
        out,
        map,
        removed_refs,
        write_string,
        direct_stream_writer,
    )
}

/// Plain live-root serializer used by xref output when the source trailer has
/// a direct Catalog. It retains the compact trailer/root layout while routing
/// nested direct streams through the same writer-owned framing hook.
pub(crate) fn unparse_object_with_ref_map_and_direct_streams(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    qdf: bool,
) -> Result<()> {
    let mut dynamic_map = |child: &ObjectHandle| {
        child
            .object_ref()
            .ok_or_else(|| Error::Internal("direct root map received a direct child".into()))
            .and_then(map)
    };
    let mut write_string =
        |out: &mut OutputSink<'_>, value: &[u8]| crate::pdf_syntax::write_string_value(out, value);
    let mut direct_stream_writer = DefaultDynamicDirectStreamWriter {
        newline_before_endstream: Some(crate::writer::NewlineBeforeEndstream::Never),
        qdf_mode: qdf,
    };
    unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
        handle,
        out,
        &mut dynamic_map,
        removed_refs,
        &mut write_string,
        &mut direct_stream_writer,
    )
}

fn unparse_child_with_dynamic_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
    direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect()
            || object_gen
                .to_object_ref()
                .is_some_and(|object_ref| removed_refs.contains(&object_ref))
        {
            out.write_bytes(b"null")?;
            return Ok(());
        }
        write_object_ref(out, map(handle)?)?;
        return Ok(());
    }
    if !handle.is_initialized() {
        handle.try_dereference()?;
    }
    match write_direct_child_with_dynamic_ref_map_and_string_writer(handle, out, write_string)? {
        DynamicDirectChildKind::Scalar => return Ok(()),
        DynamicDirectChildKind::Stream => {
            let mut write_string_wrapper =
                |out: &mut OutputSink<'_>, value: &[u8]| write_string(out, value);
            return direct_stream_writer.write_direct_stream(
                handle,
                out,
                map,
                removed_refs,
                &mut write_string_wrapper,
            );
        }
        DynamicDirectChildKind::Container => {}
    }
    unparse_object_walk_with_dynamic_ref_map_and_string_writer(
        handle,
        out,
        map,
        removed_refs,
        write_string,
        direct_stream_writer,
    )
}

enum DynamicDirectChildKind {
    Scalar,
    Container,
    Stream,
}

/// Serialize a direct scalar child without entering the recursive container
/// walker. Arrays and dictionaries return `Container`, while streams return
/// `Stream` so their existing direct-stream policy remains responsible for
/// framing and child reference discovery.
fn write_direct_child_with_dynamic_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<DynamicDirectChildKind>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    handle.with_value(|value| match value {
        Some(ObjectValue::Array(_) | ObjectValue::Dictionary(_)) => {
            Ok(DynamicDirectChildKind::Container)
        }
        Some(ObjectValue::Stream(_)) => Ok(DynamicDirectChildKind::Stream),
        Some(value) => {
            unparse_object_value_with_dynamic_ref_map_and_string_writer(value, out, write_string)?;
            Ok(DynamicDirectChildKind::Scalar)
        }
        None => {
            // cov:ignore-start: a successful dereference exposes a value; keep
            // the same defensive null spelling as the recursive walker.
            out.write_bytes(b"null")?;
            Ok(DynamicDirectChildKind::Scalar)
            // cov:ignore-end
        }
    })
}

fn unparse_object_walk_with_dynamic_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
    direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if try_write_direct_scalar_container_with_string_writer(value, out, write_string)? {
                    Ok(None)
                } else if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_with_dynamic_ref_map_and_string_writer(
                        value,
                        out,
                        write_string,
                    )
                    .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes a value; retain the defensive null fallback.
                out.write_bytes(b"null")?;
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(UnparseContainer::Array(children)) => {
                out.write_bytes(b"[")?;
                for child in children {
                    out.write_bytes(b" ")?;
                    unparse_child_with_dynamic_ref_map_and_string_writer(
                        &child,
                        out,
                        map,
                        removed_refs,
                        write_string,
                        direct_stream_writer,
                    )?; // cov:ignore: LLVM attributes the covered dynamic array-child continuation separately.
                }
                out.write_bytes(b" ]")?;
            }
            Some(UnparseContainer::Dictionary(entries)) => {
                unparse_dict_entries_with_dynamic_ref_map_and_string_writer(
                    &entries,
                    out,
                    map,
                    removed_refs,
                    write_string,
                    direct_stream_writer,
                )?; // cov:ignore: LLVM attributes the covered dynamic dictionary-child continuation separately.
            }
            Some(UnparseContainer::Stream(stream_dict)) => {
                // cov:ignore-start: direct Stream children are intercepted by the dynamic child hook; this arm is retained only for a top-level stream-dictionary fallback.
                unparse_object_walk_with_dynamic_ref_map_and_string_writer(
                    &stream_dict,
                    out,
                    map,
                    removed_refs,
                    write_string,
                    direct_stream_writer,
                )?;
                // cov:ignore-end
            }
            None => {}
        }
        Ok(())
    })
}

fn unparse_object_value_with_dynamic_ref_map_and_string_writer<F>(
    value: &ObjectValue,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

fn unparse_dict_entries_with_dynamic_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
    direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    out.write_bytes(b"<<")?;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_with_dynamic_ref_map_and_string_writer(
                value,
                out,
                map,
                removed_refs,
                write_string,
                direct_stream_writer, // cov:ignore: LLVM attributes the covered dynamic stream-child argument separately.
            )?; // cov:ignore: the dynamic stream-child serializer is exercised by the direct-stream policy regression.
        }
    }
    out.write_bytes(b" >>")
}

fn unparse_stream_dict_entries_with_dynamic_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
    direct_stream_writer: &mut dyn DynamicDirectStreamWriter,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()> + ?Sized,
{
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_with_dynamic_ref_map_and_string_writer(
                value,
                out,
                map,
                removed_refs,
                write_string,
                direct_stream_writer, // cov:ignore: LLVM attributes the covered dynamic stream-dictionary child argument separately.
            )?; // cov:ignore: the dynamic stream-dictionary child serializer is exercised by the direct-stream policy regression.
        } // cov:ignore: LLVM attributes the covered dynamic stream-dictionary branch exit to its child serializer call.
    }
    if let Some(length) = length_value {
        out.write_bytes(b" /Length ")?;
        unparse_child_with_dynamic_ref_map_and_string_writer(
            length,
            out,
            map,
            removed_refs,
            write_string,
            direct_stream_writer, // cov:ignore: LLVM attributes the covered dynamic stream-length argument separately.
        )?; // cov:ignore: the dynamic stream-length serializer is exercised by the direct-stream policy regression.
    } // cov:ignore: LLVM attributes the covered dynamic stream-length emission to the preceding callback terminator.
    if options.add_flate_filter {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")
}

fn unparse_child_with_dynamic_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    // qpdf's writer treats raw object number 0 as its null placeholder even
    // when the handle cannot cross the normal ObjectRef generation boundary;
    // do not send that placeholder into the live queue's discovery callback.
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect()
            || object_gen
                .to_object_ref()
                .is_some_and(|object_ref| removed_refs.contains(&object_ref))
        {
            out.write_bytes(b"null")?;
            return Ok(());
        }
        let mapped = map(handle)?;
        write_object_ref(out, mapped)?;
        return Ok(());
    }
    if !handle.is_initialized() {
        handle.try_dereference()?;
    }
    if write_direct_child_with_dynamic_ref_map(handle, out, map, removed_refs)? {
        return Ok(());
    }
    unparse_object_walk_with_dynamic_ref_map(handle, out, map, removed_refs)
}

fn write_direct_child_with_dynamic_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<bool> {
    handle.with_value(|value| match value {
        Some(ObjectValue::Array(_) | ObjectValue::Dictionary(_) | ObjectValue::Stream(_)) => {
            Ok(false)
        }
        Some(value) => {
            unparse_object_value_with_dynamic_ref_map(value, out, map, removed_refs)?;
            Ok(true)
        }
        None => {
            // cov:ignore-start: a successful dereference exposes a value; keep
            // the same defensive null spelling as the recursive walker.
            out.write_bytes(b"null")?;
            Ok(true)
            // cov:ignore-end
        }
    })
}

fn unparse_object_walk_with_dynamic_ref_map(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_with_dynamic_ref_map(value, out, map, removed_refs)
                        .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: with_value exposes every resolved slot as Some; this is a defensive resolver-violation fallback.
                out.write_bytes(b"null")?;
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(UnparseContainer::Array(children)) => {
                out.write_bytes(b"[")?;
                for child in children {
                    out.write_bytes(b" ")?;
                    unparse_child_with_dynamic_ref_map(&child, out, map, removed_refs)?;
                }
                out.write_bytes(b" ]")?;
            }
            Some(UnparseContainer::Dictionary(entries)) => {
                unparse_dict_entries_with_dynamic_ref_map(&entries, out, map, removed_refs)?;
            }
            Some(UnparseContainer::Stream(stream_dict)) => {
                unparse_object_walk_with_dynamic_ref_map(&stream_dict, out, map, removed_refs)?;
            }
            None => {}
        }
        Ok(())
    })
}

fn unparse_object_value_with_dynamic_ref_map(
    value: &ObjectValue,
    out: &mut OutputSink<'_>,
    _map: &mut DynamicObjectRefMap<'_>,
    _removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    // `unparse_object_walk_with_dynamic_ref_map` snapshots every container
    // before reaching this scalar fallback, so the callback has no work here.
    // Keep scalar formatting on the existing qpdf-shaped primitive.
    unparse_object_value(value, out)
}

fn unparse_dict_entries_with_dynamic_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    out.write_bytes(b"<<")?;
    for (key, value) in visible_dict_entries(entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_with_dynamic_ref_map(value, out, map, removed_refs)?; // cov:ignore: LLVM attributes this child-call terminator to callback cleanup.
        } // cov:ignore: LLVM attributes this child-call terminator to callback cleanup.
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

fn unparse_stream_dict_entries_with_dynamic_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    options: StreamDictionaryOptions,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<()> {
    let entries = prepare_stream_dict_entries(entries, options)?;
    out.write_bytes(b"<<")?;
    let mut length_value: Option<&ObjectHandle> = None;
    for (key, value) in visible_dict_entries(&entries)? {
        if is_removed_reference(value, removed_refs) {
            continue;
        }
        if key.as_slice() == b"/Length" {
            length_value = Some(value);
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(&entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_with_dynamic_ref_map(value, out, map, removed_refs)?; // cov:ignore: LLVM attributes this child-call terminator to callback cleanup.
        } // cov:ignore: LLVM attributes this child-call terminator to callback cleanup.
    }
    if let Some(length) = length_value {
        out.write_bytes(b" /Length ")?;
        unparse_child_with_dynamic_ref_map(length, out, map, removed_refs)?; // cov:ignore: LLVM attributes this length-call terminator to callback cleanup.
    } // cov:ignore: LLVM attributes this length-call terminator to callback cleanup.
    if options.add_flate_filter {
        out.write_bytes(b" /Filter /FlateDecode")?;
    }
    out.write_bytes(b" >>")?;
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
pub(crate) fn dict_is_sig_with_byte_range(entries: &[(Vec<u8>, ObjectHandle)]) -> Result<bool> {
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
// the ordinary `unparse_child`/`unparse_child_qdf` call, when `force_hex_string`
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
// Note what this ordering fix does *not* claim: unlike the
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
// here, keeps that resolution order aligned with qpdf's own single pass.
// The test
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
// `(&self, out: &mut OutputSink<'_>, ...) -> Result<()>` -- so there is no
// encryption state to route around in the first place here; wiring an
// actual encryption pipeline around these bytes is a future
// consumer-migration/encryption-integration concern this primitive does not
// implement, matching the scope limits `unparse_stream_body`/
// `write_trailer` already document for their own out-of-scope qpdf steps
// (e.g. the `t_lin_second` branch, the `/Crypt`-filter stripping logic).
fn try_write_sig_contents_hex_string(
    handle: &ObjectHandle,
    force_hex_string: bool,
    out: &mut OutputSink<'_>,
) -> Result<bool> {
    if !force_hex_string
        || handle
            .qpdf_obj_gen()
            .is_some_and(|object_gen| object_gen.is_indirect())
    {
        return Ok(false);
    }
    handle.try_dereference()?;
    handle.with_value(|value| {
        if let Some(ObjectValue::String(bytes)) = value {
            crate::pdf_syntax::write_hex_string(out, bytes)?;
            Ok(true)
        } else {
            Ok(false)
        }
    })
}

// Writes `<< /K1 v1 /K2 v2 >>` with qpdf's suppression rule applied
// (`QPDFWriter.cc:1488-1527`, non-stream case: no `/Length` tail). Matches
// `Dictionary::write_pdf`'s own key-writing shape (`object.rs:839-848`): a
// leading space, then `/` + the escaped key, pushed separately since
// `write_name_escaped` does not write the leading slash itself. Also applies
// the `/Contents`-in-a-`/Sig`-dictionary hex-string special case that same
// qpdf loop applies unconditionally (`QPDFWriter.cc:1490-1504`) -- see
// `dict_is_sig_with_byte_range`/`try_write_sig_contents_hex_string`'s own
// docs for the detection/writing split.
fn unparse_dict_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut OutputSink<'_>,
) -> Result<()> {
    out.write_bytes(b"<<")?;
    for (key, value) in visible_dict_entries(entries)? {
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child(value, out)?;
        }
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

// Append `n` ASCII space bytes to `out` — the QDF family's own copy of
// `object.rs`'s private `push_spaces` helper. Not reusable across the module
// boundary (that one is not `pub(crate)`, and this task's scope is
// `object_handle.rs` only), but the two are one-line bodies, not logic worth
// sharing at the cost of widening `object.rs`'s API for a single call site.
const SPACE_BLOCK: [u8; 64] = [b' '; 64];

fn push_spaces(out: &mut OutputSink<'_>, mut n: usize) -> Result<()> {
    while n >= SPACE_BLOCK.len() {
        out.write_bytes(&SPACE_BLOCK)?;
        n -= SPACE_BLOCK.len();
    }
    if n > 0 {
        out.write_bytes(&SPACE_BLOCK[..n])?;
    }
    Ok(())
}

// QDF-mode sibling of `unparse_child` above: an indirect child always writes
// as its own `"N G R"` reference form regardless of QDF mode — qpdf never
// inlines an indirect object at a child position in either mode, the same
// unconditional child/reference split already applies. A direct
// child recurses through `unparse_object_walk_qdf` at `indent`, the same
// column its own container already committed to for this child (an array
// element or dict value sits at its container's `indent + 2`; see
// `unparse_object_value_qdf`'s own Array/Dictionary arms for where that
// `+ 2` is actually applied before calling this).
//
// No separate reserved check either, for the identical reason `unparse_child`
// has none (see its own doc for the full trace): an *indirect* reserved
// child always takes the reference-token branch below without ever being
// dereferenced here, and a *direct* one is still rejected one level down,
// by `unparse_object_walk_qdf`'s own `is_reserved` check on whatever
// handle the `None` branch below recurses into.
fn unparse_child_qdf(handle: &ObjectHandle, indent: usize, out: &mut OutputSink<'_>) -> Result<()> {
    // Same raw-identity boundary as `unparse_child` above: QDF changes the
    // formatting, not which children are written as references.
    if let Some(object_gen) = handle
        .qpdf_obj_gen()
        .filter(|object_gen| object_gen.is_indirect())
    {
        write_qpdf_object_gen(out, object_gen)?;
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
fn unparse_object_walk_qdf(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    unparse_object_walk_hub(|| {
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
                out.write_bytes(b"null")?;
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
    out: &mut OutputSink<'_>,
) -> Result<()> {
    match container {
        UnparseContainer::Array(children) => {
            // qpdf's QDF array arm: `[`, a newline, then each
            // child at `indent + 2`, followed by the closing bracket at
            // `indent`.
            out.write_bytes(b"[")?;
            out.write_bytes(b"\n")?;
            for child in children {
                push_spaces(out, indent + 2)?;
                unparse_child_qdf(&child, indent + 2, out)?;
                out.write_bytes(b"\n")?;
            }
            push_spaces(out, indent)?;
            out.write_bytes(b"]")?;
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
// between the two modes (the qpdf QDF writer's own fallthrough to
// `self.write_pdf(out)` for everything but its three container arms is the
// same split), so this delegates that whole fallthrough set to
// `unparse_object_value` itself rather than duplicating its match arms.
fn unparse_object_value_qdf(
    value: &ObjectValue,
    indent: usize,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    match value {
        ObjectValue::Array(children) => {
            // qpdf's QDF array arm: `[`, a newline,
            // then per element `indent + 2` leading spaces + the child's own
            // QDF form + a trailing newline, then `indent` leading spaces and
            // `]`.
            // cov:ignore-start: unparse_object_walk_qdf snapshots every array into UnparseContainer before this value fallback can dispatch.
            out.write_bytes(b"[")?;
            out.write_bytes(b"\n")?;
            for child in children {
                push_spaces(out, indent + 2)?;
                unparse_child_qdf(child, indent + 2, out)?;
                out.write_bytes(b"\n")?;
            }
            push_spaces(out, indent)?;
            out.write_bytes(b"]")?;
            // cov:ignore-end
        }
        ObjectValue::Dictionary(entries) => {
            // cov:ignore-start: the QDF recursion hub snapshots every container before this fallback is entered.
            let entries: Vec<(Vec<u8>, ObjectHandle)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            unparse_dict_entries_qdf(&entries, indent, out)?;
        }
        ObjectValue::Stream(stream) => {
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
            // same value's own position, exactly as qpdf's QDF writer's
            // `Stream` arm calls `stream.dict.write_pdf_qdf(out, indent)` at
            // the unincremented indent before appending its
            // `stream`/`endstream` framing.
            unparse_object_walk_qdf(&stream.stream_dict, indent, out)?;
            // cov:ignore-end
        }
        // Every remaining scalar variant has no QDF-specific
        // framing -- reuse `unparse_object_value`'s own arms for them
        // verbatim rather than duplicating scalar-formatting logic. Spelled
        // out explicitly (rather than an `other =>` catch-all) so this match
        // stays exhaustive: adding a new `ObjectValue` variant, or removing
        // one of the three container arms above, is a compile error here
        // instead of a silent fallthrough -- the same enforcement
        // `unparse_object_value` itself already gets from having no
        // catch-all arm at all.
        ObjectValue::Null
        | ObjectValue::Unresolved
        | ObjectValue::Reserved
        | ObjectValue::Destroyed
        | ObjectValue::Boolean(_)
        | ObjectValue::Integer(_)
        | ObjectValue::Real(_)
        | ObjectValue::RealLiteral { .. }
        | ObjectValue::Name(_)
        | ObjectValue::String(_)
        | ObjectValue::Operator(_)
        | ObjectValue::InlineImage(_) => unparse_object_value(value, out)?,
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
fn unparse_dict_entries_qdf(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    out.write_bytes(b"<<\n")?;
    for (key, value) in visible_dict_entries(entries)? {
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if !try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            unparse_child_qdf(value, indent + 2, out)?;
        }
        out.write_bytes(b"\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

fn unparse_child_qdf_with_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect() || removed_refs.contains(&object_gen) {
            out.write_bytes(b"null")?;
        } else {
            write_object_ref(out, map(object_gen)?)?;
        }
        return Ok(());
    }
    unparse_object_walk_qdf_with_ref_map(handle, indent, out, map, removed_refs)
}

fn unparse_object_walk_qdf_with_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        if handle.try_is_array()? {
            return unparse_array_qdf_with_ref_map(handle, indent, out, map, removed_refs);
        }
        if handle.try_is_dictionary()? {
            return unparse_dictionary_qdf_with_ref_map(handle, indent, out, map, removed_refs);
        }
        if let Some(stream_dict) = handle.as_stream_dict() {
            return unparse_object_walk_qdf_with_ref_map(
                &stream_dict,
                indent,
                out,
                map,
                removed_refs,
            );
        }
        handle.with_value(|value| match value {
            Some(value) => {
                unparse_object_value_qdf_with_ref_map(value, indent, out, map, removed_refs)
            }
            None => {
                // cov:ignore-start: after try_dereference, a live non-reserved handle cannot expose None
                out.write_bytes(b"null")
                // cov:ignore-end
            }
        })
    })
}

fn unparse_array_qdf_with_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    out.write_bytes(b"[")?;
    out.write_bytes(b"\n")?;
    let items = handle.try_array_items()?;
    let mut cursor = items.begin();
    while !cursor.is_end() {
        push_spaces(out, indent + 2)?;
        let child = cursor.current();
        unparse_child_qdf_with_ref_map(&child, indent + 2, out, map, removed_refs)?;
        out.write_bytes(b"\n")?;
        cursor.next();
    }
    push_spaces(out, indent)?;
    out.write_bytes(b"]")
}

fn unparse_dictionary_qdf_with_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    out.write_bytes(b"<<\n")?;
    let mut current_key = LiveDictionaryKeyBuffer::default();
    let mut next_key = LiveDictionaryKeyBuffer::default();
    let mut first_entry = true;
    while let Some(value) = handle.next_dictionary_entry_for_live_walk(
        (!first_entry).then_some(current_key.as_slice()),
        &mut next_key,
    ) {
        std::mem::swap(&mut current_key, &mut next_key);
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
            || value.try_is_null()?
        {
            first_entry = false;
            continue;
        }
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, current_key.as_slice())?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            current_key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range_handle(handle)?;
        if !try_write_sig_contents_hex_string(&value, force_hex_string, out)? {
            unparse_child_qdf_with_ref_map(&value, indent + 2, out, map, removed_refs)?;
        }
        out.write_bytes(b"\n")?;
        first_entry = false;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")
}

/// Emit a QDF object while assigning indirect child references through the
/// writer's live queue at the same `unparseChild` boundary qpdf uses. This is
/// the QDF counterpart of the dynamic compact writer above: it keeps container
/// edges live, preserves dictionary null suppression, and avoids a detached
/// edge snapshot or a separate pre-walk of the member value.
pub(crate) fn unparse_object_qdf_with_dynamic_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    unparse_object_walk_qdf_with_dynamic_ref_map(handle, indent, out, map, removed_refs)
}

enum QdfDynamicContainer {
    Array,
    Dictionary,
    Stream(ObjectHandle),
    Written,
}

fn unparse_object_walk_qdf_with_dynamic_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    unparse_object_walk_hub(|| {
        handle.try_dereference()?;
        let container = handle.with_value(|value| -> Result<QdfDynamicContainer> {
            match value {
                Some(ObjectValue::Array(_)) => Ok(QdfDynamicContainer::Array),
                Some(ObjectValue::Dictionary(_)) => Ok(QdfDynamicContainer::Dictionary),
                Some(ObjectValue::Stream(stream)) => {
                    Ok(QdfDynamicContainer::Stream(stream.stream_dict.clone()))
                }
                Some(value) => {
                    unparse_object_value_qdf(value, indent, out)?;
                    Ok(QdfDynamicContainer::Written)
                }
                None => {
                    // cov:ignore-start: a successful dereference always
                    // exposes a value; retain the conservative null fallback
                    // for a resolver that violates that invariant.
                    out.write_bytes(b"null")?;
                    // cov:ignore-end
                    Ok(QdfDynamicContainer::Written) // cov:ignore: with_value always exposes the resolved value as Some after the dereference boundary
                }
            }
        })?;
        match container {
            QdfDynamicContainer::Array => {
                out.write_bytes(b"[\n")?;
                let items = handle.try_array_items()?;
                let mut cursor = items.begin();
                while !cursor.is_end() {
                    push_spaces(out, indent + 2)?;
                    let child = cursor.current();
                    unparse_child_qdf_with_dynamic_ref_map(
                        &child,
                        indent + 2,
                        out,
                        map,
                        removed_refs,
                    )?; // cov:ignore: LLVM attributes the successful QDF array-child continuation to the loop merge
                    out.write_bytes(b"\n")?;
                    cursor.next();
                }
                push_spaces(out, indent)?;
                out.write_bytes(b"]")?;
            }
            QdfDynamicContainer::Dictionary => {
                out.write_bytes(b"<<\n")?;
                let mut current_key = LiveDictionaryKeyBuffer::default();
                let mut next_key = LiveDictionaryKeyBuffer::default();
                let mut first_entry = true;
                while let Some(value) = handle.next_dictionary_entry_for_live_walk(
                    (!first_entry).then_some(current_key.as_slice()),
                    &mut next_key,
                ) {
                    std::mem::swap(&mut current_key, &mut next_key);
                    if value
                        .qpdf_obj_gen()
                        .is_some_and(|object_gen| removed_refs.contains(&object_gen))
                        || value.try_is_null()?
                    {
                        first_entry = false;
                        continue;
                    }
                    push_spaces(out, indent + 2)?;
                    write_dictionary_key(out, current_key.as_slice())?;
                    out.write_bytes(b" ")?;
                    let force_hex_string = current_key.as_slice() == b"/Contents"
                        && dict_is_sig_with_byte_range_handle(handle)?;
                    if !try_write_sig_contents_hex_string(&value, force_hex_string, out)? {
                        unparse_child_qdf_with_dynamic_ref_map(
                            &value,
                            indent + 2,
                            out,
                            map,
                            removed_refs,
                        )?; // cov:ignore: LLVM attributes the successful QDF dictionary-child continuation to the loop merge
                    }
                    out.write_bytes(b"\n")?;
                    first_entry = false;
                }
                push_spaces(out, indent)?;
                out.write_bytes(b">>")?;
            }
            QdfDynamicContainer::Stream(stream_dict) => {
                unparse_object_walk_qdf_with_dynamic_ref_map(
                    &stream_dict,
                    indent,
                    out,
                    map,
                    removed_refs,
                )?; // cov:ignore: LLVM attributes the successful QDF stream-dictionary continuation to the loop merge
            }
            QdfDynamicContainer::Written => {}
        }
        Ok(())
    })
}

fn unparse_child_qdf_with_dynamic_ref_map(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &mut DynamicObjectRefMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect() || removed_refs.contains(&object_gen) {
            out.write_bytes(b"null")?;
        } else {
            write_object_ref(out, map(handle)?)?;
        }
        return Ok(());
    }
    let direct_value = handle.with_value(|value| -> Result<Option<QdfDynamicContainer>> {
        match value {
            Some(ObjectValue::Array(_))
            | Some(ObjectValue::Dictionary(_))
            | Some(ObjectValue::Stream(_)) => Ok(None),
            Some(value) => {
                unparse_object_value_qdf(value, indent, out)?;
                Ok(Some(QdfDynamicContainer::Written))
            }
            None => {
                // cov:ignore-start: direct handles always carry a value;
                // retain the conservative null fallback for a malformed slot.
                out.write_bytes(b"null")?;
                // cov:ignore-end
                Ok(Some(QdfDynamicContainer::Written)) // cov:ignore: with_value always exposes the direct value as Some
            }
        }
    })?;
    if direct_value.is_some() {
        return Ok(());
    }
    unparse_object_walk_qdf_with_dynamic_ref_map(handle, indent, out, map, removed_refs)
}

fn dict_is_sig_with_byte_range_handle(handle: &ObjectHandle) -> Result<bool> {
    let type_value = handle.try_get_key(b"/Type")?;
    if !type_value.try_is_name_and_equals(b"Sig")? {
        return Ok(false);
    }
    let byte_range = handle.try_get_key(b"/ByteRange")?;
    Ok(!byte_range.try_is_null()?)
}

fn unparse_object_value_qdf_with_ref_map(
    value: &ObjectValue,
    _indent: usize,
    out: &mut OutputSink<'_>,
    _map: &QpdfObjGenMap<'_>,
    _removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    // `unparse_object_walk_qdf_with_ref_map` snapshots every array,
    // dictionary, and stream before entering this borrow-scoped fallback.
    // A bare reference is handled there as well, before this function is
    // called. The only values that can reach this helper are therefore
    // scalar payloads, whose QDF spelling is identical to the compact form.
    unparse_object_value(value, out)
}

fn unparse_child_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect() || removed_refs.contains(&object_gen) {
            out.write_bytes(b"null")?;
        } else {
            write_object_ref(out, map(object_gen)?)?;
        }
        return Ok(());
    }
    unparse_object_walk_with_ref_map_and_string_writer(handle, out, map, removed_refs, write_string)
}

fn unparse_object_walk_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_with_ref_map_and_string_writer(
                        value,
                        out,
                        map,
                        removed_refs,
                        write_string,
                    )
                    .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: after try_dereference, a live non-reserved handle cannot expose None
                out.write_bytes(b"null")?;
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_with_ref_map_and_string_writer(
                container,
                out,
                map,
                removed_refs,
                write_string,
            ),
            None => Ok(()),
        }
    })
}

fn unparse_container_with_ref_map_and_string_writer<F>(
    container: UnparseContainer,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.write_bytes(b"[")?;
            for child in children {
                out.write_bytes(b" ")?;
                unparse_child_with_ref_map_and_string_writer(
                    &child,
                    out,
                    map,
                    removed_refs,
                    write_string,
                )?; // cov:ignore: LLVM maps the covered child call continuation to this line
            }
            out.write_bytes(b" ]")?;
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_with_ref_map_and_string_writer(
                &entries,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered dictionary call continuation to this line
        }
        UnparseContainer::Stream(stream_dict) => {
            // cov:ignore-start: production stream writers handle stream framing before this generic static string-writer fallback.
            unparse_object_walk_with_ref_map_and_string_writer(
                &stream_dict,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered stream-dictionary call continuation to this line
                // cov:ignore-end
        }
    }
    Ok(())
}

fn unparse_object_value_with_ref_map_and_string_writer<F>(
    value: &ObjectValue,
    out: &mut OutputSink<'_>,
    _map: &QpdfObjGenMap<'_>,
    _removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

fn unparse_dict_entries_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    out.write_bytes(b"<<")?;
    for (key, value) in visible_dict_entries(entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            continue;
        }
        unparse_child_with_ref_map_and_string_writer(value, out, map, removed_refs, write_string)?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

fn unparse_child_qdf_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    if let Some(object_gen) = handle.qpdf_obj_gen() {
        if !object_gen.is_indirect() || removed_refs.contains(&object_gen) {
            out.write_bytes(b"null")?;
        } else {
            write_object_ref(out, map(object_gen)?)?;
        }
        return Ok(());
    }
    unparse_object_walk_qdf_with_ref_map_and_string_writer(
        handle,
        indent,
        out,
        map,
        removed_refs,
        write_string,
    )
}

fn unparse_object_walk_qdf_with_ref_map_and_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    unparse_object_walk_hub(|| {
        // cov:ignore: reserved precondition closure has no independent LLVM counter
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_qdf_with_ref_map_and_string_writer(
                        value,
                        indent,
                        out,
                        map,
                        removed_refs,
                        write_string,
                    )
                    .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: after try_dereference, a live non-reserved handle cannot expose None
                out.write_bytes(b"null")?;
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_qdf_with_ref_map_and_string_writer(
                container,
                indent,
                out,
                map,
                removed_refs,
                write_string,
            ),
            None => Ok(()),
        }
    })
}

fn unparse_container_qdf_with_ref_map_and_string_writer<F>(
    container: UnparseContainer,
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.write_bytes(b"[")?;
            out.write_bytes(b"\n")?;
            for child in children {
                push_spaces(out, indent + 2)?;
                unparse_child_qdf_with_ref_map_and_string_writer(
                    &child,
                    indent + 2,
                    out,
                    map,
                    removed_refs,
                    write_string,
                )?; // cov:ignore: LLVM maps the covered child call continuation to this line
                out.write_bytes(b"\n")?;
            }
            push_spaces(out, indent)?;
            out.write_bytes(b"]")?;
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf_with_ref_map_and_string_writer(
                &entries,
                indent,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered dictionary call continuation to this line
        }
        UnparseContainer::Stream(stream_dict) => {
            // cov:ignore-start: production stream writers handle stream framing before this generic QDF static string-writer fallback.
            unparse_object_walk_qdf_with_ref_map_and_string_writer(
                &stream_dict,
                indent,
                out,
                map,
                removed_refs,
                write_string,
            )?; // cov:ignore: LLVM maps the covered stream-dictionary call continuation to this line
                // cov:ignore-end
        }
    }
    Ok(())
}

fn unparse_object_value_qdf_with_ref_map_and_string_writer<F>(
    value: &ObjectValue,
    _indent: usize,
    out: &mut OutputSink<'_>,
    _map: &QpdfObjGenMap<'_>,
    _removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

fn unparse_dict_entries_qdf_with_ref_map_and_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    out.write_bytes(b"<<\n")?;
    for (key, value) in visible_dict_entries(entries)? {
        if value
            .qpdf_obj_gen()
            .is_some_and(|object_gen| removed_refs.contains(&object_gen))
        {
            continue;
        }
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_hex_string(value, force_hex_string, out)? {
            out.write_bytes(b"\n")?;
            continue;
        }
        unparse_child_qdf_with_ref_map_and_string_writer(
            value,
            indent + 2,
            out,
            map,
            removed_refs,
            write_string,
        )?; // cov:ignore: LLVM maps the covered mapped dictionary child call continuation to this line
        out.write_bytes(b"\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

#[cfg(test)]
fn unparse_child_with_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    if let Some(object_ref) = handle.object_ref() {
        write_object_ref(out, object_ref)?;
        return Ok(());
    }
    unparse_object_walk_with_string_writer(handle, out, write_string)
}

#[cfg(test)]
fn unparse_container_with_string_writer<F>(
    container: UnparseContainer,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.write_bytes(b"[")?;
            for child in children {
                out.write_bytes(b" ")?;
                unparse_child_with_string_writer(&child, out, write_string)?;
            }
            out.write_bytes(b" ]")?;
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_with_string_writer(&entries, out, write_string)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_with_string_writer(&stream_dict, out, write_string)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn unparse_object_walk_with_string_writer<F>(
    handle: &ObjectHandle,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_with_string_writer(value, out, write_string).map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes Null for
                // the null fallback or errors while unresolved.
                out.write_bytes(b"null")?;
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => unparse_container_with_string_writer(container, out, write_string),
            None => Ok(()),
        }
    })
}

#[cfg(test)]
fn unparse_object_value_with_string_writer<F>(
    value: &ObjectValue,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

#[cfg(test)]
fn try_write_sig_contents_with_string_writer(
    handle: &ObjectHandle,
    force_hex_string: bool,
    out: &mut OutputSink<'_>,
) -> Result<bool> {
    if !force_hex_string || handle.object_ref().is_some() {
        return Ok(false);
    }
    handle.try_dereference()?;
    let bytes = handle.with_value(|value| match value {
        Some(ObjectValue::String(bytes)) => Some(bytes.clone()),
        _ => None,
    });
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    // QPDFWriter.cc:1501 adds f_no_encryption together with f_hex_string for
    // signature contents. The ordinary string callback is therefore bypassed
    // here: qpdf keeps this value cleartext and only changes its spelling to
    // hexadecimal, even while the surrounding object is encrypted.
    crate::pdf_syntax::write_hex_string(out, &bytes)?;
    Ok(true)
}

#[cfg(test)]
fn unparse_dict_entries_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    out.write_bytes(b"<<")?;
    for (key, value) in visible_dict_entries(entries)? {
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            continue;
        }
        unparse_child_with_string_writer(value, out, write_string)?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

#[cfg(test)]
fn unparse_child_qdf_with_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    if let Some(object_ref) = handle.object_ref() {
        write_object_ref(out, object_ref)?;
        return Ok(());
    }
    unparse_object_walk_qdf_with_string_writer(handle, indent, out, write_string)
}

#[cfg(test)]
fn unparse_container_qdf_with_string_writer<F>(
    container: UnparseContainer,
    indent: usize,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match container {
        UnparseContainer::Array(children) => {
            out.write_bytes(b"[")?;
            out.write_bytes(b"\n")?;
            for child in children {
                push_spaces(out, indent + 2)?;
                unparse_child_qdf_with_string_writer(&child, indent + 2, out, write_string)?;
                out.write_bytes(b"\n")?;
            }
            push_spaces(out, indent)?;
            out.write_bytes(b"]")?;
        }
        UnparseContainer::Dictionary(entries) => {
            unparse_dict_entries_qdf_with_string_writer(&entries, indent, out, write_string)?;
        }
        UnparseContainer::Stream(stream_dict) => {
            unparse_object_walk_qdf_with_string_writer(&stream_dict, indent, out, write_string)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn unparse_object_walk_qdf_with_string_writer<F>(
    handle: &ObjectHandle,
    indent: usize,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    unparse_object_walk_hub(|| {
        if handle.is_reserved() {
            return Err(reserved_unparse_error());
        }
        handle.try_dereference()?;
        let container = handle.with_value(|value| match value {
            Some(value) => {
                if let Some(container) = snapshot_unparse_container(value) {
                    Ok(Some(container))
                } else {
                    unparse_object_value_qdf_with_string_writer(value, indent, out, write_string)
                        .map(|()| None)
                }
            }
            None => {
                // cov:ignore-start: successful dereference exposes Null for
                // the null fallback or errors while unresolved.
                out.write_bytes(b"null")?;
                Ok(None)
                // cov:ignore-end
            }
        })?;
        match container {
            Some(container) => {
                unparse_container_qdf_with_string_writer(container, indent, out, write_string)
            }
            None => Ok(()),
        }
    })
}

#[cfg(test)]
fn unparse_object_value_qdf_with_string_writer<F>(
    value: &ObjectValue,
    _indent: usize,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    match value {
        ObjectValue::String(bytes) => write_string(out, bytes),
        _ => unparse_object_value(value, out),
    }
}

#[cfg(test)]
fn unparse_dict_entries_qdf_with_string_writer<F>(
    entries: &[(Vec<u8>, ObjectHandle)],
    indent: usize,
    out: &mut OutputSink<'_>,
    write_string: &mut F,
) -> Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> Result<()>,
{
    out.write_bytes(b"<<\n")?;
    for (key, value) in visible_dict_entries(entries)? {
        push_spaces(out, indent + 2)?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        let force_hex_string =
            key.as_slice() == b"/Contents" && dict_is_sig_with_byte_range(entries)?;
        if try_write_sig_contents_with_string_writer(value, force_hex_string, out)? {
            out.write_bytes(b"\n")?;
            continue;
        }
        unparse_child_qdf_with_string_writer(value, indent + 2, out, write_string)?;
        out.write_bytes(b"\n")?;
    }
    push_spaces(out, indent)?;
    out.write_bytes(b">>")?;
    Ok(())
}

// `write_trailer`'s sole callee. Writes the (already-trimmed,
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
#[cfg(test)]
fn unparse_trailer_entries(
    entries: &[(Vec<u8>, ObjectHandle)],
    xref_stream: bool,
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    if !xref_stream {
        out.write_bytes(b"trailer <<")?;
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
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        unparse_child(value, out)?;
    }
    if let Some(value) = id_value {
        out.write_bytes(b" /ID ")?;
        match id_writer.as_mut() {
            Some(write_id) => write_id(out)?,
            None => write_id_style_value_handle(value, out)?,
        }
    }
    if let Some(value) = encrypt_value {
        out.write_bytes(b" /Encrypt ")?;
        unparse_child(value, out)?;
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // mirrors writeTrailer's independent layout, ID, mapping, and visibility controls
fn unparse_trailer_entries_with_ref_map(
    entries: &[(Vec<u8>, ObjectHandle)],
    xref_stream: bool,
    qdf: bool,
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    let qpdf_map = qpdf_obj_gen_map_from_object_ref_map(map);
    let qpdf_removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
    if qdf {
        out.write_bytes(b"trailer <<\n")?;
    } else if !xref_stream {
        out.write_bytes(b"trailer <<")?;
    }

    let mut id_value: Option<&ObjectHandle> = None;
    let mut encrypt_value: Option<&ObjectHandle> = None;
    for (key, value) in entries {
        // `/ID` and `/Encrypt` are installed by the writer in output space.
        // They must not be discarded because their output reference happens
        // to reuse an object number recorded in the source-side removal set.
        // qpdf's writeTrailer handles these writer-owned keys separately from
        // the ordinary trailer-child filtering (`QPDFWriter.cc:1174-1192`).
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
        // qpdf's writeTrailer always emits the writer-owned /Root
        // (QPDFWriter.cc:1160-1236 applies no null/removed filtering). Its value
        // is the output-space Catalog installed by build_writer_trailer_handle,
        // so source null/removed filtering must never drop it — e.g. when the
        // source Catalog is not object 1 and source object 1 is null/free/removed
        // and the Catalog renumbers onto output object 1.
        let writer_owned_root = key.as_slice() == b"/Root";
        if !writer_owned_root && suppress_null_values && value.try_is_null()? {
            continue;
        }
        if !writer_owned_root && is_removed_reference(value, removed_refs) {
            continue;
        }

        if qdf {
            out.write_bytes(b"  ")?;
        } else {
            out.write_bytes(b" ")?;
        }
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        if key.as_slice() == b"/Root" && value.object_ref().is_none() {
            // An inline Catalog is writer-owned, but its indirect descendants
            // remain in source space until this final child walk. qpdf's
            // `unparseChild` recurses into that direct dictionary, so preserve
            // the direct `/Root` shape while applying the caller's map below.
            if qdf {
                unparse_child_qdf_with_ref_map(value, 2, out, &qpdf_map, &qpdf_removed_refs)?;
            } else {
                unparse_child_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?;
            }
        } else if matches!(key.as_slice(), b"/Root" | b"/Encrypt") {
            // An indirect `/Root` or `/Encrypt` installed by the writer already
            // carries an output-space reference and must not be remapped again.
            if qdf {
                unparse_child_qdf(value, 2, out)?; // cov:ignore: writer-owned indirect /Root or /Encrypt references are emitted through the dedicated xref-stream path in production.
            } else {
                unparse_child(value, out)?;
            }
        } else if qdf {
            unparse_child_qdf_with_ref_map(value, 2, out, &qpdf_map, &qpdf_removed_refs)?;
        } else {
            unparse_child_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?;
        }
        if qdf {
            out.write_bytes(b"\n")?;
        }
    }

    if let Some(value) = id_value {
        if qdf {
            out.write_bytes(b"  /ID ")?;
        } else {
            out.write_bytes(b" /ID ")?;
        }
        match id_writer.as_mut() {
            Some(write_id) => write_id(out)?,
            None => {
                write_id_style_value_handle_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?
            }
        }
    }
    if let Some(value) = encrypt_value {
        out.write_bytes(b" /Encrypt ")?;
        unparse_child(value, out)?;
    }

    if qdf {
        if id_value.is_some() || encrypt_value.is_some() {
            out.write_bytes(b"\n")?;
        }
        out.write_bytes(b">>\n")?;
    } else {
        out.write_bytes(b" >>")?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn unparse_trailer_entries_with_ref_map_and_kind(
    entries: &[(Vec<u8>, ObjectHandle)],
    kind: TrailerKind,
    xref_stream: bool,
    qdf: bool,
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
    direct_root: Option<&ObjectHandle>,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    let qpdf_map = qpdf_obj_gen_map_from_object_ref_map(map);
    let qpdf_removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
    let (size, prev, second_half) = match kind {
        TrailerKind::Normal { size } => (size, None, false),
        TrailerKind::LinearizedFirst { size, prev } => (size, Some(prev), false),
        TrailerKind::LinearizedSecond { size } => (size, None, true),
    };

    // qpdf writes the `trailer` keyword only for the classic (non-xref-stream)
    // form, then unconditionally emits the QDF separator newline regardless of
    // which branch ran (`if (xref_stream) {...} else {writeString("trailer
    // <<");} writeStringQDF("\n");`, `QPDFWriter.cc:1159-1169`). Folding the
    // `qdf` check into the same branch as `xref_stream` (as an `else if`)
    // would drop the "trailer <<" keyword's separator on the embedded
    // xref-stream QDF form -- the classic route's callers do not run that
    // combination, but `xref_stream: true` from `write_xref_stream` does.
    if !xref_stream {
        out.write_bytes(b"trailer <<")?;
    }
    if qdf {
        out.write_bytes(b"\n")?;
    }

    // qpdf's t_lin_second form writes the writer-owned /Size before walking
    // the trimmed source trailer, so it is present even when the input
    // trailer had no literal /Size key (QPDFWriter.cc:1170-1172).
    if second_half {
        if qdf {
            out.write_bytes(b"  /Size ")?;
        } else {
            out.write_bytes(b" /Size ")?;
        }
        write_decimal_i64(out, size)?;
        if qdf {
            out.write_bytes(b"\n")?;
        }
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
                if !second_half {
                    encrypt_value = Some(value);
                }
                continue;
            }
            b"/Prev" => continue,
            _ if second_half && key.as_slice() == b"/Size" => continue,
            _ if second_half && key.as_slice() != b"/Size" => continue,
            _ => {}
        }

        if key.as_slice() == b"/Size" {
            // qpdf's `getKeys()` omits a null-valued key
            // (`QPDF_Dictionary.cc:getKeys`), so `writeTrailer` never sees a
            // `/Size null` entry and emits no computed size for it. This arm
            // runs before the general null suppression below, so it has to make
            // the same test itself.
            if suppress_null_values && value.try_is_null()? {
                continue;
            }
            if qdf {
                out.write_bytes(b"  ")?;
            } else {
                out.write_bytes(b" ")?;
            }
            write_dictionary_key(out, key)?;
            out.write_bytes(b" ")?;
            write_decimal_i64(out, size)?;
            if let Some(prev) = prev {
                out.write_bytes(b" /Prev ")?;
                write_decimal_u64(out, prev)?;
                let padding = 21usize.saturating_sub(decimal_u64_len(prev));
                push_spaces(out, padding)?;
            }
            if qdf {
                out.write_bytes(b"\n")?;
            }
            continue;
        }

        // qpdf's writeTrailer always emits the writer-owned /Root
        // (QPDFWriter.cc:1160-1236 applies no null/removed filtering). Its value
        // is the output-space Catalog installed by build_writer_trailer_handle,
        // so source null/removed filtering must never drop it — e.g. when the
        // source Catalog is not object 1 and source object 1 is null/free/removed
        // and the Catalog renumbers onto output object 1.
        let writer_owned_root = key.as_slice() == b"/Root";
        if !writer_owned_root && suppress_null_values && value.try_is_null()? {
            continue;
        }
        if !writer_owned_root && is_removed_reference(value, removed_refs) {
            continue;
        }
        if qdf {
            out.write_bytes(b"  ")?;
        } else {
            out.write_bytes(b" ")?;
        }
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        if key.as_slice() == b"/Root" && value.object_ref().is_none() {
            if qdf {
                unparse_child_qdf_with_ref_map(value, 2, out, &qpdf_map, &qpdf_removed_refs)?;
            } else if let Some(direct_root) = direct_root {
                unparse_object_with_ref_map_and_direct_streams(
                    direct_root,
                    out,
                    map,
                    removed_refs,
                    false,
                )?; // cov:ignore: LLVM maps this covered direct-root serializer continuation to the call setup.
            } else {
                unparse_child_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?;
            }
        } else if key.as_slice() == b"/Root" {
            if qdf {
                unparse_child_qdf(value, 2, out)?;
            } else {
                unparse_child(value, out)?;
            }
        } else if qdf {
            unparse_child_qdf_with_ref_map(value, 2, out, &qpdf_map, &qpdf_removed_refs)?;
        } else {
            unparse_child_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?;
        }
        if qdf {
            out.write_bytes(b"\n")?;
        }
    }

    if let Some(value) = id_value {
        if qdf {
            out.write_bytes(b"  /ID ")?;
        } else {
            out.write_bytes(b" /ID ")?;
        }
        match id_writer.as_mut() {
            Some(write_id) => write_id(out)?,
            None => {
                write_id_style_value_handle_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?
            }
        }
    }
    if let Some(value) = encrypt_value {
        out.write_bytes(b" /Encrypt ")?;
        unparse_child(value, out)?;
    }
    if qdf {
        if id_value.is_some() || encrypt_value.is_some() {
            out.write_bytes(b"\n")?;
        }
        out.write_bytes(b">>\n")?;
    } else {
        out.write_bytes(b" >>")?;
    }
    Ok(())
}

#[cfg(test)]
fn unparse_dictionary_entries_with_ref_map_and_id_writer(
    entries: &[(Vec<u8>, ObjectHandle)],
    mut id_writer: Option<crate::pdf_syntax::TrailerIdWriter>,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    suppress_null_values: bool,
    out: &mut OutputSink<'_>,
) -> Result<()> {
    let qpdf_map = qpdf_obj_gen_map_from_object_ref_map(map);
    let qpdf_removed_refs = qpdf_obj_gen_set_from_object_ref_set(removed_refs)?;
    out.write_bytes(b"<<")?;
    for (key, value) in entries {
        // qpdf's writeTrailer always emits the writer-owned /Root
        // (QPDFWriter.cc:1160-1236 applies no null/removed filtering). Its value
        // is the output-space Catalog installed by build_writer_trailer_handle,
        // so source null/removed filtering must never drop it — e.g. when the
        // source Catalog is not object 1 and source object 1 is null/free/removed
        // and the Catalog renumbers onto output object 1.
        let writer_owned_root = key.as_slice() == b"/Root";
        if !writer_owned_root && suppress_null_values && value.try_is_null()? {
            continue;
        }
        if !writer_owned_root && is_removed_reference(value, removed_refs) {
            continue;
        }
        out.write_bytes(b" ")?;
        write_dictionary_key(out, key)?;
        out.write_bytes(b" ")?;
        if key.as_slice() == b"/ID" {
            match id_writer.as_mut() {
                Some(write_id) => write_id(out)?,
                None => write_id_style_value_handle_with_ref_map(
                    value,
                    out,
                    &qpdf_map,
                    &qpdf_removed_refs,
                )?, // cov:ignore: llvm-cov does not attribute this test-only /ID fallback continuation to the exercised call.
            }
        } else if matches!(key.as_slice(), b"/Root" | b"/Encrypt") {
            unparse_child(value, out)?; // cov:ignore: test-only dictionary serializer receives writer-owned indirect references only in defensive unit shapes.
        } else {
            unparse_child_with_ref_map(value, out, &qpdf_map, &qpdf_removed_refs)?;
        }
    }
    out.write_bytes(b" >>")?;
    Ok(())
}

// Writes a trailer's `/ID` value in qpdf's `writeTrailer` compact shape:
// `[<hex1><hex2>]`, no spaces (`QPDFWriter.cc:1194-1222`, `/ID [` then the
// two identifier strings via `QPDF_String::unparse(true)`, then `]`).
// Mirrors qpdf's identifier writer byte-for-byte, but walks `value`'s own
// `ObjectHandle` shape directly: an indirect `value` (an `/ID` array stored as
// a reference -- not a shape real qpdf itself ever produces, but nothing
// at the type level rules it out) writes as its own `"N G R"` form via
// `unparse_child`, checked before any shape inspection, matching
// `unparse_child`'s own reference-vs-recurse split and never inlining an
// indirect value regardless of what it resolves to. A direct
// `Array([String, String])` gets the compact hex-pair form; any other
// direct shape (wrong arity, non-string elements) falls back to
// `unparse_child`'s generic form rather than silently truncating -- the
// same "fall back, don't truncate" choice `write_id_style_value` makes.
#[cfg(test)]
fn write_id_style_value_handle(value: &ObjectHandle, out: &mut OutputSink<'_>) -> Result<()> {
    if value.object_ref().is_some() {
        return unparse_child(value, out);
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
            out.write_bytes(b"[")?;
            crate::pdf_syntax::write_hex_string(out, &b0)?;
            crate::pdf_syntax::write_hex_string(out, &b1)?;
            out.write_bytes(b"]")?;
            Ok(())
        }
        None => unparse_child(value, out),
    }
}

fn write_id_style_value_handle_with_ref_map(
    value: &ObjectHandle,
    out: &mut OutputSink<'_>,
    map: &QpdfObjGenMap<'_>,
    removed_refs: &BTreeSet<QpdfObjGen>,
) -> Result<()> {
    if value
        .qpdf_obj_gen()
        .is_some_and(|object_gen| object_gen.is_indirect())
    {
        return unparse_child_with_ref_map(value, out, map, removed_refs);
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
            out.write_bytes(b"[")?;
            crate::pdf_syntax::write_hex_string(out, &b0)?;
            crate::pdf_syntax::write_hex_string(out, &b1)?;
            out.write_bytes(b"]")?;
            Ok(())
        }
        None => unparse_object_walk_with_ref_map(value, out, map, removed_refs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::rc::Rc;

    #[test]
    fn scalar_writer_uses_stack_decimal_emission() {
        let source = include_str!("object.rs").replace("\r\n", "\n");
        let start = source
            .find("pub(crate) fn unparse_object_value")
            .expect("scalar writer");
        let body = &source[start
            ..source[start..]
                .find("\n\ntype QpdfObjGenMap")
                .expect("scalar writer end")
                + start];

        assert!(
            body.contains("ObjectValue::Integer(v) => write_decimal_i64"),
            "writer scalar integers must use the stack decimal helper"
        );
        assert!(
            !body.contains("ObjectValue::Integer(v) => out.write_bytes(v.to_string().as_bytes())"),
            "writer scalar integers must not allocate a temporary String"
        );
    }

    #[test]
    fn qdf_mapped_serializer_does_not_snapshot_container_edges() {
        let source = include_str!("object.rs");
        let start = source
            .find("fn unparse_object_walk_qdf_with_ref_map")
            .expect("qdf mapped serializer");
        let body = &source[start
            ..source[start..]
                .find("\nfn unparse_array_qdf_with_ref_map")
                .expect("qdf mapped serializer end")
                + start];

        assert!(
            body.contains("unparse_array_qdf_with_ref_map"),
            "qdf mapped serializer must walk live arrays"
        );
        assert!(
            body.contains("unparse_dictionary_qdf_with_ref_map"),
            "qdf mapped serializer must walk live dictionaries"
        );
        assert!(
            !body.contains("snapshot_unparse_container"),
            "qdf mapped serializer must not clone a second container snapshot"
        );
    }

    #[test]
    fn compact_mapped_serializer_does_not_snapshot_container_edges() {
        let source = include_str!("object.rs").replace("\r\n", "\n");
        let start = source
            .find("fn unparse_object_walk_with_ref_map")
            .expect("compact mapped serializer");
        let body = &source[start
            ..source[start..]
                .find("\n\nenum RefMapContainer")
                .expect("compact mapped serializer end")
                + start];

        assert!(
            body.contains("unparse_array_with_ref_map"),
            "compact mapped serializer must walk live arrays"
        );
        assert!(
            body.contains("unparse_dictionary_with_ref_map"),
            "compact mapped serializer must walk live dictionaries"
        );
        assert!(
            !body.contains("snapshot_unparse_container"),
            "compact mapped serializer must not clone a second container snapshot"
        );
    }

    #[test]
    fn qdf_indentation_batches_space_runs() {
        let source = include_str!("object.rs").replace("\r\n", "\n");
        let start = source
            .find("fn push_spaces")
            .expect("qdf indentation helper");
        let body = &source[start
            ..source[start..]
                .find("\n\n// QDF-mode sibling")
                .expect("qdf indentation helper end")
                + start];

        assert!(
            body.contains("SPACE_BLOCK") || body.contains("SPACES"),
            "qdf indentation must write static space blocks"
        );
        assert!(
            !body.contains("for _ in 0..n"),
            "qdf indentation must not issue one output call per space"
        );
    }

    #[test]
    fn qdf_indentation_writes_large_space_runs_in_blocks() -> Result<()> {
        let mut output = Vec::new();
        super::super::output::with_buffer_sink(&mut output, |out| push_spaces(out, 128))?;
        assert_eq!(output, vec![b' '; 128]);
        Ok(())
    }

    #[test]
    fn child_walkers_reject_uninitialized_handles_at_their_boundaries() {
        let handle = ObjectHandle::uninitialized();
        let map = |_: QpdfObjGen| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        let removed = BTreeSet::new();

        let mut ordinary = Vec::new();
        assert!(
            super::super::output::with_buffer_sink(&mut ordinary, |out| {
                unparse_child(&handle, out)
            })
            .is_err()
        );

        let mut mapped = Vec::new();
        assert!(super::super::output::with_buffer_sink(&mut mapped, |out| {
            unparse_child_with_ref_map(&handle, out, &map, &removed)
        })
        .is_err());

        let mut dynamic_map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        let mut dynamic = Vec::new();
        assert!(super::super::output::with_buffer_sink(&mut dynamic, |out| {
            unparse_child_with_dynamic_ref_map(&handle, out, &mut dynamic_map, &BTreeSet::new())
        })
        .is_err());

        let mut string_map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };
        let mut direct_stream_writer = DefaultDynamicDirectStreamWriter {
            newline_before_endstream: None,
            qdf_mode: false,
        };
        let mut string_probe = Vec::new();
        super::super::output::with_buffer_sink(&mut string_probe, |out| strings(out, b"probe"))
            .expect("dynamic string callback should write direct strings");
        assert_eq!(string_probe, b"(probe)");
        let mut dynamic_strings = Vec::new();
        assert!(
            super::super::output::with_buffer_sink(&mut dynamic_strings, |out| {
                unparse_child_with_dynamic_ref_map_and_string_writer(
                    &handle,
                    out,
                    &mut string_map,
                    &BTreeSet::new(),
                    &mut strings,
                    &mut direct_stream_writer,
                )
            })
            .is_err()
        );
    }

    /// A raw object-zero identity must keep taking the slow path.
    ///
    /// qpdf treats object number 0 as non-indirect, so such a handle answers
    /// `is_direct() == true`; the child writer nonetheless emits `null` for
    /// it (`!object_gen.is_indirect()`). If the direct-scalar fast path
    /// accepted it, the array would serialize the value instead and the two
    /// routes would disagree byte for byte.
    #[test]
    fn object_zero_child_serializes_as_null_through_the_scalar_container_route() -> Result<()> {
        let zero = ObjectHandle::new_indirect_unresolved(ObjectRef::new(0, 0), -1);
        zero.set_resolved(ObjectValue::Integer(42));
        assert!(zero.is_direct(), "qpdf reports object zero as non-indirect");
        assert!(
            zero.qpdf_obj_gen().is_some(),
            "the raw identity survives that predicate"
        );

        // Include a direct string so the slow path's string callback runs on
        // the same array, pinning that the fallback keeps using it.
        let array = ObjectHandle::array(vec![
            zero,
            ObjectHandle::integer(7),
            ObjectHandle::string(b"s".to_vec()),
        ]);
        let mut map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };
        let mut direct_stream_writer = DefaultDynamicDirectStreamWriter {
            newline_before_endstream: None,
            qdf_mode: false,
        };
        let mut bytes = Vec::new();
        super::super::output::with_buffer_sink(&mut bytes, |out| {
            unparse_child_with_dynamic_ref_map_and_string_writer(
                &array,
                out,
                &mut map,
                &BTreeSet::new(),
                &mut strings,
                &mut direct_stream_writer,
            )
        })?;
        assert_eq!(bytes, b"[ null 7 (s) ]");
        Ok(())
    }

    #[test]
    fn qdf_dynamic_writer_walks_direct_streams_and_skips_null_or_removed_children() -> Result<()> {
        let removed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Null".to_vec(), ObjectHandle::null()),
                (b"/Removed".to_vec(), removed.clone()),
                (
                    b"/Array".to_vec(),
                    ObjectHandle::array(vec![removed.clone()]),
                ),
                (b"/Keep".to_vec(), ObjectHandle::integer(2)),
            ]),
            Rc::new(b"payload".to_vec()),
        );
        let removed_refs = BTreeSet::from([QpdfObjGen::new(9, 0)]);
        let mut map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(9, 0));
        let mut output = Vec::new();
        super::super::output::with_buffer_sink(&mut output, |out| {
            unparse_object_qdf_with_dynamic_ref_map(&stream, 0, out, &mut map, &removed_refs)
        })?;
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("/Keep"));
        assert!(text.contains("/Array"));
        assert!(!text.contains("/Null"));
        assert!(!text.contains("/Removed"));
        assert!(text.contains("null"));

        let mut scalar_output = Vec::new();
        let mut scalar_map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        super::super::output::with_buffer_sink(&mut scalar_output, |out| {
            unparse_object_qdf_with_dynamic_ref_map(
                &ObjectHandle::integer(3),
                0,
                out,
                &mut scalar_map,
                &BTreeSet::new(),
            )
        })?;
        assert_eq!(scalar_output, b"3");
        Ok(())
    }

    #[test]
    fn dictionary_key_writer_preserves_qpdfs_first_byte() {
        let mut out = Vec::new();
        super::super::output::with_buffer_sink(&mut out, |out| {
            write_dictionary_key(out, b"/Canonical")?;
            out.write_bytes(b" ")?;
            write_dictionary_key(out, b"Raw")
        })
        .unwrap();
        assert_eq!(out, b"/Canonical Raw");
    }

    #[test]
    fn object_ref_map_adapter_rejects_an_unprojectable_raw_identity() {
        let map = |object_ref: ObjectRef| Ok::<ObjectRef, Error>(object_ref);
        let raw_map = super::qpdf_obj_gen_map_from_object_ref_map(&map);
        let error = raw_map(QpdfObjGen::new(5, 65_536))
            .expect_err("an ObjectRef adapter must reject an out-of-range generation");
        assert!(error
            .to_string()
            .contains("cannot be used by an ObjectRef map"));
    }

    #[test]
    fn raw_object_writer_keeps_array_null_and_drops_removed_dictionary_key() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let kept = pdf.get_object_handle_by_raw_identity(6, 65_536);
        kept.set_resolved(ObjectValue::Integer(7));
        let removed = pdf.get_object_handle_by_raw_identity(5, 65_536);
        let value = ObjectHandle::dictionary(vec![
            (
                b"/Array".to_vec(),
                ObjectHandle::array(vec![removed.clone()]),
            ),
            (b"/Kept".to_vec(), kept.clone()),
            (b"/Removed".to_vec(), removed.clone()),
        ]);
        let removed_refs = BTreeSet::from([QpdfObjGen::new(5, 65_536)]);
        let mut output = Vec::new();

        super::super::output::with_buffer_sink(&mut output, |out| {
            ObjectWriterEmission::unparse_object_with_qpdf_obj_gen_map_and_removed(
                &value,
                out,
                &|object_gen| {
                    assert_eq!(object_gen, QpdfObjGen::new(6, 65_536));
                    Ok(ObjectRef::new(7, 0))
                },
                &removed_refs,
            )
        })?;

        assert_eq!(output, b"<< /Array [ null ] /Kept 7 0 R >>");
        Ok(())
    }

    #[test]
    fn compact_stream_ref_map_rejects_a_reserved_object() {
        let reserved = ObjectHandle::new_reserved_direct();
        let error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            ObjectWriterEmission::unparse_stream_body_with_ref_map_and_removed_with_options(
                &reserved,
                out,
                StreamDictionaryOptions::preserve(),
                &|_| Ok(ObjectRef::new(1, 0)), // cov:ignore: reserved validation returns before invoking the map callback.
                &BTreeSet::new(),
            )
        })
        .expect_err("reserved stream objects must be rejected");
        assert!(error.to_string().contains("reserved object"));
    }

    #[test]
    fn writer_name_emission_uses_canonical_keys_without_the_legacy_bridge() {
        let source = include_str!("object.rs");
        let legacy_name = ["legacy", "dictionary_key"].join("_");
        assert!(
            !source.contains(&legacy_name),
            "writer name emission must consume canonical slash-prefixed keys directly"
        );
    }

    #[test]
    fn dictionary_ref_map_writer_accepts_a_trailer_id_callback() -> Result<()> {
        let dictionary = ObjectHandle::dictionary(vec![
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"old-a".to_vec()),
                    ObjectHandle::string(b"old-b".to_vec()),
                ]),
            ),
            (b"/Keep".to_vec(), ObjectHandle::integer(1)),
        ]);
        let mut output = Vec::new();
        let mut write_id = |out: &mut OutputSink<'_>| out.write_bytes(b"[<01><02>]");
        let map = |object_ref: ObjectRef| Ok(object_ref);
        super::super::output::with_buffer_sink(&mut output, |out| {
            dictionary.write_dictionary_with_ref_map_and_id_writer(
                out,
                Some(&mut write_id),
                &map,
                &BTreeSet::new(),
                false,
            )
        })?; // cov:ignore: LLVM maps this successful generic dictionary call continuation to test cleanup
        assert_eq!(output, b"<< /ID [<01><02>] /Keep 1 >>");
        Ok(())
    }

    #[test]
    fn mapped_stream_writer_overrides_length_and_suppresses_null_children() -> Result<()> {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), ObjectHandle::integer(99)),
                (b"/Null".to_vec(), ObjectHandle::null()),
                (b"/Keep".to_vec(), ObjectHandle::integer(1)),
            ]),
            Rc::new(b"abc".to_vec()),
        );
        let mut output = Vec::new();
        let removed = BTreeSet::new();
        let map = |object_ref: ObjectRef| Ok(object_ref);

        super::super::output::with_buffer_sink(&mut output, |out| {
            stream.unparse_stream_body_with_ref_map_and_removed_and_length(
                out, false, &map, &removed, 3,
            )
        })?; // cov:ignore: LLVM attributes this successful stream-emission continuation to the test call cleanup.

        assert_eq!(output, b"<< /Keep 1 /Length 3 >>");

        let dictionary =
            ObjectHandle::dictionary(vec![(b"/Keep".to_vec(), ObjectHandle::integer(1))]);
        let mut dictionary_output = Vec::new();
        super::super::output::with_buffer_sink(&mut dictionary_output, |out| {
            dictionary.unparse_stream_body_with_ref_map_and_removed_and_length(
                out, false, &map, &removed, 2,
            )
        })?; // cov:ignore: LLVM attributes this successful dictionary-emission continuation to the test call cleanup.
        assert_eq!(dictionary_output, b"<< /Keep 1 /Length 2 >>");

        let scalar = ObjectHandle::integer(1);
        let mut scalar_output = Vec::new();
        super::super::output::with_buffer_sink(&mut scalar_output, |out| {
            scalar.unparse_stream_body_with_ref_map_and_removed_and_length(
                out, false, &map, &removed, 1,
            )
        })?; // cov:ignore: LLVM attributes this successful scalar-emission continuation to the test call cleanup.
        assert_eq!(scalar_output, b"<< /Length 1 >>");

        let malformed_stream =
            ObjectHandle::stream(ObjectHandle::integer(1), Rc::new(b"x".to_vec()));
        let mut malformed_output = Vec::new();
        super::super::output::with_buffer_sink(&mut malformed_output, |out| {
            malformed_stream.unparse_stream_body_with_ref_map_and_removed_and_length(
                out, false, &map, &removed, 1,
            )
        })?; // cov:ignore: LLVM attributes this successful malformed-stream fallback continuation to test cleanup.
        assert_eq!(malformed_output, b"<< /Length 1 >>");

        let reserved = ObjectHandle::new_reserved_direct();
        // cov:ignore-start: reserved validation returns before invoking this callback.
        let error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            reserved.unparse_stream_body_with_ref_map_and_removed_and_length(
                out, false, &map, &removed, 0,
            )
        })
        .expect_err("reserved stream emission must be rejected");
        assert!(error.to_string().contains("reserved object"));
        // cov:ignore-end
        Ok(())
    }

    #[test]
    fn live_mapped_stream_writer_overrides_length_without_rebuilding_dictionary() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let mapped = pdf.get_object_handle_by_raw_identity(7, 0);
        mapped.set_resolved(ObjectValue::Integer(2));
        let dictionary = ObjectHandle::dictionary(vec![
            (b"/Length".to_vec(), ObjectHandle::integer(99)),
            (b"/Null".to_vec(), ObjectHandle::null()),
            (b"/Keep".to_vec(), ObjectHandle::integer(1)),
            (b"/Ref".to_vec(), mapped),
        ]);
        let map = |object_gen: QpdfObjGen| {
            Ok::<ObjectRef, Error>(object_gen.to_object_ref().expect("test identity maps"))
        };
        let mut output = Vec::new();
        super::super::output::with_buffer_sink(&mut output, |out| {
            ObjectWriterEmission::unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
                &dictionary,
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
                3,
            )
        })?;
        assert_eq!(output, b"<< /Keep 1 /Ref 7 0 R /Length 3 >>");

        let scalar = ObjectHandle::integer(5);
        assert!(dictionary_entry_handle(&scalar, b"/Missing").is_none());
        let mut scalar_output = Vec::new();
        super::super::output::with_buffer_sink(&mut scalar_output, |out| {
            ObjectWriterEmission::unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
                &scalar,
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
                1,
            )
        })?;
        assert_eq!(scalar_output, b"<< /Length 1 >>");

        let signature = ObjectHandle::dictionary(vec![
            (
                b"/ByteRange".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(0), ObjectHandle::integer(1)]),
            ),
            (
                b"/Contents".to_vec(),
                ObjectHandle::string(vec![0x01, 0xab]),
            ),
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        ]);
        let mut signature_output = Vec::new();
        super::super::output::with_buffer_sink(&mut signature_output, |out| {
            ObjectWriterEmission::unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
                &signature,
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
                2,
            )
        })?;
        assert_eq!(
            signature_output,
            b"<< /ByteRange [ 0 1 ] /Contents <01ab> /Type /Sig /Length 2 >>"
        );

        let non_string_signature = ObjectHandle::dictionary(vec![
            (
                b"/ByteRange".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(0), ObjectHandle::integer(1)]),
            ),
            (b"/Contents".to_vec(), ObjectHandle::integer(7)),
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
        ]);
        let mut non_string_signature_output = Vec::new();
        super::super::output::with_buffer_sink(&mut non_string_signature_output, |out| {
            ObjectWriterEmission::unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
                &non_string_signature,
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
                1,
            )
        })?;
        assert_eq!(
            non_string_signature_output,
            b"<< /ByteRange [ 0 1 ] /Contents 7 /Type /Sig /Length 1 >>"
        );

        let reserved = ObjectHandle::new_reserved_direct();
        let reserved_error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            ObjectWriterEmission::unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
                &reserved,
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
                0,
            )
        })
        .expect_err("reserved live stream dictionaries must be rejected");
        assert!(reserved_error.to_string().contains("reserved object"));
        Ok(())
    }

    #[test]
    fn length_override_snapshot_fallback_drops_source_length_before_null_probe() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let mapped = pdf.get_object_handle_by_raw_identity(7, 0);
        mapped.set_resolved(ObjectValue::Integer(2));
        let dangling_length = ObjectHandle::new_indirect_unresolved(ObjectRef::new(91, 0), -1);
        let dictionary = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
                ]),
            ),
            (b"/Length".to_vec(), dangling_length),
            (b"/Ref".to_vec(), mapped),
        ]);

        let map = |object_gen: QpdfObjGen| {
            Ok::<ObjectRef, Error>(object_gen.to_object_ref().expect("test identity maps"))
        };
        let mut output = Vec::new();
        super::super::output::with_buffer_sink(&mut output, |out| {
            ObjectWriterEmission::unparse_stream_body_with_qpdf_obj_gen_map_and_removed_with_options_and_length(
                &dictionary,
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
                3,
            )
        })?;
        assert_eq!(
            output,
            b"<< /Filter [ /FlateDecode /ASCIIHexDecode ] /Ref 7 0 R /Length 3 >>"
        );
        Ok(())
    }

    #[test]
    fn stream_dictionary_owner_handles_missing_crypt_parameters_and_qdf_filter_append() -> Result<()>
    {
        let pdf = Pdf::empty()?;
        let stream = pdf.new_stream_with_data(Rc::new(b"abc".to_vec()))?;
        stream.as_stream_dict().unwrap().replace_key(
            b"/Filter",
            ObjectHandle::array(vec![
                ObjectHandle::name(b"Crypt".to_vec()),
                ObjectHandle::name(b"ASCIIHexDecode".to_vec()),
            ]),
        )?; // cov:ignore: test setup mutation has no independent branch
        let map = |object_ref: ObjectRef| Ok(object_ref);
        let missing_decode_error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            stream.unparse_stream_body_with_ref_map_and_removed_with_options(
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
            )
        })
        .expect_err("qpdf propagates a missing DecodeParms erase warning");
        assert_eq!(
            missing_decode_error.to_string(),
            " -> dictionary key /DecodeParms: operation for array attempted on object of type null: ignoring attempt to erase item"
        );
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/DecodeParms", ObjectHandle::array(Vec::new()))?;
        let mut compact = Vec::new();
        super::super::output::with_buffer_sink(&mut compact, |out| {
            stream.unparse_stream_body_with_ref_map_and_removed_with_options(
                out,
                StreamDictionaryOptions::preserve(),
                &map,
                &BTreeSet::new(),
            )
        })?; // cov:ignore: qdf/compact policy owner call is covered by the surrounding assertions; llvm-cov attributes this terminator to the callback cleanup.
        assert!(String::from_utf8_lossy(&compact).contains("/Filter [ /ASCIIHexDecode ]"));

        let policy = StreamDictionaryOptions::new(true, true);
        let mut qdf = Vec::new();
        super::super::output::with_buffer_sink(&mut qdf, |out| {
            stream.unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
                out,
                0,
                &map,
                &BTreeSet::new(),
                None,
                policy,
            )
        })?; // cov:ignore: qdf policy owner call is covered by the surrounding assertions; llvm-cov attributes this terminator to the callback cleanup.
        let qdf_text = String::from_utf8(qdf).unwrap();
        assert!(qdf_text.contains("/Filter /FlateDecode"));
        assert!(!qdf_text.contains("ASCIIHexDecode"));

        // cov:ignore-start: test-only callback body forwards strings without an independent branch
        let mut qdf_string = Vec::new();
        let mut callback = |out: &mut OutputSink<'_>, value: &[u8]| out.write_bytes(value);
        // cov:ignore-end
        super::super::output::with_buffer_sink(&mut qdf_string, |out| {
            stream.unparse_stream_body_qdf_with_ref_map_and_removed_and_length_with_string_writer_with_options(
                out,
                0,
                &map,
                &BTreeSet::new(),
                None,
                policy,
                &mut callback,
            )
        })?; // cov:ignore: qdf string policy owner call is covered by the surrounding assertions; llvm-cov attributes this terminator to callback cleanup.
        assert!(String::from_utf8(qdf_string)
            .unwrap()
            .contains("/Filter /FlateDecode"));
        Ok(())
    }

    #[test]
    fn dynamic_ref_map_emits_arrays_and_stream_dictionary_children() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let child = pdf.make_indirect_object_handle(ObjectHandle::integer(42))?;
        let removed_child = pdf.make_indirect_object_handle(ObjectHandle::integer(9))?;
        let zero_ref = ObjectHandle::new_indirect_unresolved(ObjectRef::new(0, 0), -1);
        let array = ObjectHandle::array(vec![
            child.clone(),
            removed_child.clone(),
            zero_ref,
            ObjectHandle::integer(7),
            ObjectHandle::string(b"dynamic-string".to_vec()),
        ]);
        let mut mapped = Vec::new();
        let mut map = |handle: &ObjectHandle| {
            let object_ref = handle.object_ref().expect("dynamic child is indirect");
            mapped.push(object_ref);
            Ok(object_ref)
        };
        let mut output = Vec::new();
        let removed = [removed_child.object_ref().unwrap()].into_iter().collect();
        super::super::output::with_buffer_sink(&mut output, |out| {
            array.unparse_object_with_dynamic_ref_map(out, &mut map, &removed)
        })?;
        assert_eq!(mapped, vec![child.object_ref().unwrap()]);
        assert!(String::from_utf8_lossy(&output).contains(&child.object_ref().unwrap().to_string()));
        assert!(String::from_utf8_lossy(&output).contains("null"));

        let mut string_output = Vec::new();
        let mut string_map = |handle: &ObjectHandle| {
            Ok::<ObjectRef, Error>(
                handle
                    .object_ref()
                    .expect("dynamic string child is indirect"),
            )
        };
        let mut write_string = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };
        let mut direct_stream_writer = DefaultDynamicDirectStreamWriter {
            newline_before_endstream: Some(crate::writer::NewlineBeforeEndstream::Never),
            qdf_mode: false,
        };
        super::super::output::with_buffer_sink(&mut string_output, |out| {
            unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                &array,
                out,
                &mut string_map,
                &removed,
                &mut write_string,
                &mut direct_stream_writer,
            )
        })?;
        assert!(String::from_utf8_lossy(&string_output).contains("null"));

        let reserved = ObjectHandle::new_reserved_direct();
        // cov:ignore-start: reserved validation returns before invoking this callback.
        let error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            reserved.unparse_object_with_dynamic_ref_map(
                out,
                &mut |_| Ok(ObjectRef::new(1, 0)),
                &BTreeSet::new(),
            )
        })
        .expect_err("reserved dynamic object must be rejected");
        // cov:ignore-end
        assert!(error.to_string().contains("reserved object"));

        let dictionary = ObjectHandle::dictionary(vec![
            (b"/Removed".to_vec(), removed_child.clone()),
            (b"/Mapped".to_vec(), child.clone()),
        ]);
        let mut dictionary_output = Vec::new();
        let mut dictionary_map =
            |handle: &ObjectHandle| Ok(handle.object_ref().expect("dictionary child is indirect"));
        super::super::output::with_buffer_sink(&mut dictionary_output, |out| {
            dictionary.unparse_object_with_dynamic_ref_map(out, &mut dictionary_map, &removed)
        })?; // cov:ignore: LLVM attributes this dictionary-call terminator to callback cleanup.
        let dictionary_text = String::from_utf8_lossy(&dictionary_output);
        assert!(dictionary_text.contains("/Mapped"));
        assert!(!dictionary_text.contains("/Removed"));

        let mut string_dictionary_output = Vec::new();
        super::super::output::with_buffer_sink(&mut string_dictionary_output, |out| {
            unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                &dictionary,
                out,
                &mut string_map,
                &removed,
                &mut write_string,
                &mut direct_stream_writer,
            )
        })?;
        assert!(!String::from_utf8_lossy(&string_dictionary_output).contains("/Removed"));

        let stream_length = pdf.make_indirect_object_handle(ObjectHandle::integer(4))?;
        let stream_child = pdf.make_indirect_object_handle(ObjectHandle::integer(8))?;
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), stream_length),
                (b"/Child".to_vec(), stream_child),
            ]),
            Rc::new(b"body".to_vec()),
        );
        let mut stream_object_output = Vec::new();
        let mut stream_object_map =
            |handle: &ObjectHandle| Ok(handle.object_ref().expect("stream child is indirect"));
        super::super::output::with_buffer_sink(&mut stream_object_output, |out| {
            stream.unparse_object_with_dynamic_ref_map(
                out,
                &mut stream_object_map,
                &BTreeSet::new(),
            )
        })?; // cov:ignore: LLVM attributes this stream-call terminator to callback cleanup.
        assert!(String::from_utf8_lossy(&stream_object_output).contains("/Child"));

        let stream = pdf.new_stream_with_data(Rc::new(b"body".to_vec()))?;
        let stream_ref_child = pdf.make_indirect_object_handle(ObjectHandle::integer(11))?;
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Length", stream_ref_child.clone())?;
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Child", child.clone())?;
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Label", ObjectHandle::string(b"dynamic-label".to_vec()))?;
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Removed", removed_child.clone())?;
        let mut stream_output = Vec::new();
        let mut stream_map =
            |handle: &ObjectHandle| Ok(handle.object_ref().expect("dynamic child is indirect"));
        super::super::output::with_buffer_sink(&mut stream_output, |out| {
            stream
                .as_stream_dict()
                .unwrap()
                .unparse_stream_body_with_dynamic_ref_map(
                    out,
                    StreamDictionaryOptions::new(false, true),
                    &mut stream_map,
                    &removed,
                )
        })?; // cov:ignore: LLVM attributes this stream-body call terminator to callback cleanup.
        assert!(String::from_utf8_lossy(&stream_output).contains("/Length"));
        assert!(String::from_utf8_lossy(&stream_output).contains("/Filter /FlateDecode"));

        let mut dynamic_string_output = Vec::new();
        let mut dynamic_string_map =
            |handle: &ObjectHandle| Ok(handle.object_ref().expect("dynamic length is indirect"));
        let mut dynamic_string_writer = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };
        super::super::output::with_buffer_sink(&mut dynamic_string_output, |out| {
            stream
                .as_stream_dict()
                .unwrap()
                .unparse_stream_body_with_dynamic_ref_map_and_string_writer(
                    out,
                    StreamDictionaryOptions::new(false, true),
                    &mut dynamic_string_map,
                    &removed,
                    &mut dynamic_string_writer,
                    &mut direct_stream_writer,
                )
        })?;
        let dynamic_string_text = String::from_utf8_lossy(&dynamic_string_output);
        assert!(dynamic_string_text.contains("/Length"));
        assert!(dynamic_string_text.contains("/Filter /FlateDecode"));
        Ok(())
    }

    #[test]
    fn default_dynamic_direct_stream_writer_obeys_qdf_and_newline_policies() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let child = pdf.make_indirect_object_handle(ObjectHandle::integer(7))?;
        let child_ref = child.object_ref().expect("direct stream child identity");
        let inner = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), ObjectHandle::integer(99)),
                (
                    b"/Label".to_vec(),
                    ObjectHandle::string(b"direct-label".to_vec()),
                ),
                (b"/Child".to_vec(), child),
            ]),
            Rc::new(b"direct-payload".to_vec()),
        );
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Nested".to_vec(),
                ObjectHandle::array(vec![inner]),
            )]),
            Rc::new(b"outer-payload".to_vec()),
        );

        for (qdf_mode, newline_before_endstream) in [
            (false, crate::writer::NewlineBeforeEndstream::Yes),
            (true, crate::writer::NewlineBeforeEndstream::Never),
        ] {
            let value = ObjectHandle::array(vec![stream.clone()]);
            let mut output = Vec::new();
            let mut map = |handle: &ObjectHandle| {
                Ok::<ObjectRef, Error>(handle.object_ref().expect("direct stream child identity"))
            };
            let mut write_string = |out: &mut OutputSink<'_>, value: &[u8]| {
                crate::pdf_syntax::write_string_value(out, value)
            };
            let mut direct_stream_writer = DefaultDynamicDirectStreamWriter {
                newline_before_endstream: Some(newline_before_endstream),
                qdf_mode,
            };
            super::super::output::with_buffer_sink(&mut output, |out| {
                unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                    &value,
                    out,
                    &mut map,
                    &BTreeSet::new(),
                    &mut write_string,
                    &mut direct_stream_writer,
                )
            })?;
            let text = String::from_utf8_lossy(&output);
            assert!(text.contains("/Label (direct-label)"));
            assert!(text.contains(&format!("/Child {child_ref}")));
            assert!(text.contains("direct-payload\nendstream"));
        }
        Ok(())
    }

    #[test]
    fn array_writers_cover_compact_qdf_mapping_and_removed_reference_shapes() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let kept = pdf.make_indirect_object_handle(ObjectHandle::integer(1))?;
        let removed = pdf.make_indirect_object_handle(ObjectHandle::integer(2))?;
        let removed_refs = [removed.object_ref().unwrap()].into_iter().collect();
        let array = ObjectHandle::array(vec![
            ObjectHandle::integer(7),
            kept.clone(),
            removed.clone(),
        ]);

        let mut compact = Vec::new();
        super::super::output::with_buffer_sink(&mut compact, |out| {
            ObjectWriterEmission::unparse_object(&array, out)
        })?;
        assert!(String::from_utf8_lossy(&compact).contains("[ 7"));

        let map = |object_ref: ObjectRef| {
            assert_eq!(object_ref, kept.object_ref().unwrap());
            Ok(ObjectRef::new(21, 0))
        };
        let mut mapped = Vec::new();
        super::super::output::with_buffer_sink(&mut mapped, |out| {
            ObjectWriterEmission::unparse_object_with_ref_map_and_removed(
                &array,
                out,
                &map,
                &removed_refs,
            )
        })?;
        assert_eq!(mapped, b"[ 7 21 0 R null ]");

        let mut qdf = Vec::new();
        super::super::output::with_buffer_sink(&mut qdf, |out| {
            array.unparse_object_qdf_with_ref_map_and_removed(out, 2, &map, &removed_refs)
        })?;
        assert_eq!(qdf, b"[\n    7\n    21 0 R\n    null\n  ]");

        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            out.write_bytes(b"<string:")?;
            out.write_bytes(value)?;
            out.write_bytes(b">")
        };
        let string_array =
            ObjectHandle::array(vec![ObjectHandle::string(b"value".to_vec()), removed]);
        let mut qdf_strings = Vec::new();
        super::super::output::with_buffer_sink(&mut qdf_strings, |out| {
            string_array.unparse_object_qdf_with_ref_map_and_removed_with_string_writer(
                out,
                0,
                &map,
                &removed_refs,
                &mut strings,
            )
        })?;
        assert!(String::from_utf8_lossy(&qdf_strings).contains("<string:value>"));
        assert!(String::from_utf8_lossy(&qdf_strings).contains("null"));

        let mut plain_qdf_strings = Vec::new();
        let mut plain_qdf_callback = |out: &mut OutputSink<'_>, value: &[u8]| {
            out.write_bytes(b"<string:")?;
            out.write_bytes(value)?;
            out.write_bytes(b">")
        };
        super::super::output::with_buffer_sink(&mut plain_qdf_strings, |out| {
            ObjectWriterEmission::unparse_object_qdf_with_string_writer(
                &string_array,
                out,
                0,
                &mut plain_qdf_callback,
            )
        })?;
        assert!(String::from_utf8_lossy(&plain_qdf_strings).contains("<string:value>"));
        assert!(String::from_utf8_lossy(&plain_qdf_strings).contains("[\n"));
        Ok(())
    }

    #[test]
    fn compact_string_writer_emits_removed_array_reference_as_null() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let kept = pdf.make_indirect_object_handle(ObjectHandle::integer(1))?;
        let kept_ref = kept.object_ref().unwrap();
        let removed = pdf.make_indirect_object_handle(ObjectHandle::integer(2))?;
        let removed_refs = [removed.object_ref().unwrap()].into_iter().collect();
        let array =
            ObjectHandle::array(vec![kept, removed, ObjectHandle::string(b"value".to_vec())]);
        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            out.write_bytes(b"<string:")?;
            out.write_bytes(value)?;
            out.write_bytes(b">")
        };
        let mut output = Vec::new();

        super::super::output::with_buffer_sink(&mut output, |out| {
            array.unparse_object_with_ref_map_and_removed_with_string_writer(
                out,
                &|object_ref| {
                    assert_eq!(object_ref, kept_ref);
                    Ok(ObjectRef::new(12, 0))
                },
                &removed_refs,
                &mut strings,
            )
        })?;

        assert_eq!(output, b"[ 12 0 R null <string:value> ]");
        Ok(())
    }

    #[test]
    fn qdf_string_writer_keeps_signature_contents_hex_and_formats_arrays() -> Result<()> {
        let mut pdf = Pdf::empty()?;
        let mapped = pdf.make_indirect_object_handle(ObjectHandle::integer(9))?;
        let signature = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (
                b"/ByteRange".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(0), ObjectHandle::integer(1)]),
            ),
            (b"/Contents".to_vec(), ObjectHandle::string(vec![0, 0xff])),
            (b"/Label".to_vec(), ObjectHandle::string(b"label".to_vec())),
            (b"/Mapped".to_vec(), mapped),
        ]);
        let mut output = Vec::new();
        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };

        super::super::output::with_buffer_sink(&mut output, |out| {
            signature.unparse_object_qdf_with_ref_map_and_removed_with_string_writer(
                out,
                0,
                &|object_ref| Ok(object_ref),
                &BTreeSet::new(),
                &mut strings,
            )
        })?;

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("/ByteRange [\n"));
        assert!(text.contains("/Contents <00ff>"));
        assert!(text.contains("/Label (label)"));
        Ok(())
    }

    #[test]
    fn dynamic_string_writer_fast_path_keeps_signature_hex_and_drops_nulls() -> Result<()> {
        let signature = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"/ByteRange".to_vec(), ObjectHandle::integer(0)),
            (b"/Contents".to_vec(), ObjectHandle::string(vec![0, 0xff])),
            (b"/Label".to_vec(), ObjectHandle::string(b"label".to_vec())),
            (b"/Null".to_vec(), ObjectHandle::null()),
        ]);
        let mut output = Vec::new();
        let mut map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };
        let mut direct_stream_writer = DefaultDynamicDirectStreamWriter {
            newline_before_endstream: None,
            qdf_mode: false,
        };

        super::super::output::with_buffer_sink(&mut output, |out| {
            unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                &signature,
                out,
                &mut map,
                &BTreeSet::new(),
                &mut strings,
                &mut direct_stream_writer,
            )
        })?;

        let text = String::from_utf8(output).expect("signature output is ASCII");
        assert!(text.contains("/Contents <00ff>"));
        assert!(text.contains("/Label (label)"));
        assert!(!text.contains("/Null"));

        let non_string_contents = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Sig".to_vec())),
            (b"/ByteRange".to_vec(), ObjectHandle::integer(0)),
            (b"/Contents".to_vec(), ObjectHandle::integer(7)),
        ]);
        let mut fallback_output = Vec::new();
        let mut fallback_map = |_: &ObjectHandle| Ok::<ObjectRef, Error>(ObjectRef::new(1, 0));
        let mut fallback_stream_writer = DefaultDynamicDirectStreamWriter {
            newline_before_endstream: None,
            qdf_mode: false,
        };
        super::super::output::with_buffer_sink(&mut fallback_output, |out| {
            unparse_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                &non_string_contents,
                out,
                &mut fallback_map,
                &BTreeSet::new(),
                &mut strings,
                &mut fallback_stream_writer,
            )
        })?;
        assert!(String::from_utf8_lossy(&fallback_output).contains("/Contents 7"));
        Ok(())
    }

    #[test]
    fn mapped_id_writer_falls_back_to_general_object_emission() -> Result<()> {
        let value = ObjectHandle::integer(7);
        let mut output = Vec::new();
        let map = |object_gen: QpdfObjGen| {
            Ok::<ObjectRef, Error>(object_gen.to_object_ref().expect("valid test identity"))
        };
        assert_eq!(map(QpdfObjGen::new(1, 0))?, ObjectRef::new(1, 0));
        super::super::output::with_buffer_sink(&mut output, |out| {
            super::write_id_style_value_handle_with_ref_map(&value, out, &map, &BTreeSet::new())
        })?;
        assert_eq!(output, b"7");
        Ok(())
    }

    #[test]
    fn encrypted_string_ref_map_rejects_a_reserved_object() {
        let reserved = ObjectHandle::new_reserved_direct();
        // cov:ignore-start: reserved validation returns before invoking this test callback.
        let mut strings = |out: &mut OutputSink<'_>, value: &[u8]| {
            crate::pdf_syntax::write_string_value(out, value)
        };
        // cov:ignore-end
        let error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            reserved.unparse_object_with_ref_map_and_removed_with_string_writer(
                out,
                &|object_ref| Ok(object_ref), // cov:ignore: reserved validation returns before invoking this test callback.
                &BTreeSet::new(),
                &mut strings,
            )
        })
        .expect_err("reserved encrypted-string object must be rejected");
        assert!(error.to_string().contains("reserved object"));
    }

    #[test]
    fn trailer_id_serializer_uses_the_ref_map_fallback() -> Result<()> {
        let entries = vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"left".to_vec()),
                ObjectHandle::string(b"right".to_vec()),
            ]),
        )];
        let mut output = Vec::new();
        super::super::output::with_buffer_sink(&mut output, |out| {
            super::unparse_dictionary_entries_with_ref_map_and_id_writer(
                &entries,
                None,
                &|object_ref| Ok(object_ref), // cov:ignore: the direct /ID array uses the compact fallback and never maps a reference.
                &BTreeSet::new(),
                false,
                out,
            )
        })?;
        assert_eq!(output, b"<< /ID [<6c656674><7269676874>] >>");
        Ok(())
    }

    #[test]
    fn mapped_id_serializer_handles_indirect_and_malformed_values() -> Result<()> {
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        let mut indirect_output = Vec::new();
        super::super::output::with_buffer_sink(&mut indirect_output, |out| {
            super::write_id_style_value_handle_with_ref_map(
                &indirect,
                out,
                &|object_gen| {
                    Ok(ObjectRef::new(
                        u32::try_from(object_gen.get_obj()).unwrap() + 1,
                        0,
                    ))
                },
                &BTreeSet::new(),
            )
        })?;
        assert_eq!(indirect_output, b"10 0 R");

        let malformed = ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::string(b"value".to_vec()),
        ]);
        let mut malformed_output = Vec::new();
        super::super::output::with_buffer_sink(&mut malformed_output, |out| {
            super::write_id_style_value_handle_with_ref_map(
                &malformed,
                out,
                &|_: QpdfObjGen| Ok(ObjectRef::new(1, 0)), // cov:ignore: malformed direct /ID values use generic emission without mapping a reference.
                &BTreeSet::new(),
            )
        })?;
        assert_eq!(malformed_output, b"[ 1 (value) ]");
        Ok(())
    }

    #[test]
    fn compact_trailer_adapter_skips_a_removed_reference() -> Result<()> {
        let removed = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), -1);
        let entries = vec![(b"/Removed".to_vec(), removed)];
        let removed_refs = [ObjectRef::new(9, 0)].into_iter().collect();
        let map = |object_ref| Ok::<ObjectRef, Error>(object_ref);
        assert_eq!(map(ObjectRef::new(1, 0))?, ObjectRef::new(1, 0));
        let mut output = Vec::new();
        super::super::output::with_buffer_sink(&mut output, |out| {
            super::unparse_dictionary_entries_with_ref_map_and_id_writer(
                &entries,
                None,
                &map,
                &removed_refs,
                false,
                out,
            )
        })?;
        assert_eq!(output, b"<< >>");

        let null_entries = vec![(b"/Null".to_vec(), ObjectHandle::null())];
        let mut null_output = Vec::new();
        super::super::output::with_buffer_sink(&mut null_output, |out| {
            super::unparse_dictionary_entries_with_ref_map_and_id_writer(
                &null_entries,
                None,
                &map,
                &BTreeSet::new(),
                true,
                out,
            )
        })?;
        assert_eq!(null_output, b"<< >>");
        Ok(())
    }

    #[test]
    fn qdf_trailer_with_ref_map_emits_direct_root_id_encrypt_and_custom_values() -> Result<()> {
        let root = ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Catalog".to_vec()),
        )]);
        let trailer = ObjectHandle::dictionary(vec![
            (b"/Root".to_vec(), root),
            (b"/Custom".to_vec(), ObjectHandle::integer(9)),
            (
                b"/Removed".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1),
            ),
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"a".to_vec()),
                    ObjectHandle::string(b"b".to_vec()),
                ]),
            ),
            (
                b"/Encrypt".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(8, 0), -1),
            ),
        ]);
        let mut output = Vec::new();
        let map = |object_ref| Ok(object_ref);
        let removed_refs = [ObjectRef::new(10, 0)].into_iter().collect();

        super::super::output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map(out, false, true, None, &map, &removed_refs, false)
        })?;

        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("trailer <<\n"));
        assert!(text.contains("  /Root <<\n"));
        assert!(text.contains("  /Custom 9\n"));
        assert!(text.contains("  /ID [<61><62>] /Encrypt 8 0 R\n"));
        assert!(text.contains(" /Encrypt 8 0 R\n>>\n"));
        assert!(!text.contains("/Removed"));
        Ok(())
    }

    #[test]
    fn classic_trailer_with_ref_map_emits_direct_root_id_encrypt_and_custom_values() -> Result<()> {
        let root = ObjectHandle::dictionary(vec![(
            b"/Type".to_vec(),
            ObjectHandle::name(b"Catalog".to_vec()),
        )]);
        let trailer = ObjectHandle::dictionary(vec![
            (b"/Root".to_vec(), root),
            (b"/Custom".to_vec(), ObjectHandle::integer(9)),
            (
                b"/Removed".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1),
            ),
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"a".to_vec()),
                    ObjectHandle::string(b"b".to_vec()),
                ]),
            ),
            (
                b"/Encrypt".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(8, 0), -1),
            ),
        ]);
        let mut output = Vec::new();
        let map = |object_ref| Ok(object_ref);
        let removed_refs = [ObjectRef::new(10, 0)].into_iter().collect();

        super::super::output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map(out, false, false, None, &map, &removed_refs, false)
        })?;

        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("trailer <<"));
        assert!(!text.starts_with("trailer <<\n"));
        assert!(text.contains(" /Root <<"));
        assert!(text.contains(" /Custom 9"));
        assert!(text.contains(" /ID [<61><62>] /Encrypt 8 0 R"));
        assert!(text.ends_with(" >>"));
        assert!(!text.contains("\n"));
        assert!(!text.contains("/Removed"));
        Ok(())
    }

    #[test]
    fn classic_trailer_with_ref_map_emits_an_indirect_root_by_reference() -> Result<()> {
        let trailer = ObjectHandle::dictionary(vec![(
            b"/Root".to_vec(),
            ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), -1),
        )]);
        let mut output = Vec::new();
        let map = |object_ref| Ok(object_ref);
        let removed_refs = BTreeSet::new();

        super::super::output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map(out, false, false, None, &map, &removed_refs, false)
        })?;

        let text = String::from_utf8(output).unwrap();
        assert_eq!(text, "trailer << /Root 1 0 R >>");
        Ok(())
    }

    // Two direct dictionaries holding each other: `a` under `/B` holds `b`,
    // and `b` under `/A` holds `a`. Neither handle is indirect, so no walk
    // ever meets the indirect boundary that normally terminates a descent.
    // `replace_key` refuses only the single-hop self-insert, so this two-hop
    // shape does close into a real cycle -- asserted here so the walk
    // assertions below cannot pass against an unaliased pair.
    fn reciprocal_direct_dictionary_cycle() -> Result<ObjectHandle> {
        let a = ObjectHandle::dictionary(vec![]);
        let b = ObjectHandle::dictionary(vec![]);
        a.replace_key(b"/B", b.clone())?;
        b.replace_key(b"/A", a.clone())?;
        assert!(
            a.try_get_key(b"/B")?
                .try_get_key(b"/A")?
                .is_same_object_as(&a),
            "the reciprocal replace_key pair must close into a direct cycle"
        );
        Ok(a)
    }

    // `n` nested direct dictionaries around a scalar leaf. Only the
    // dictionaries enter a walk hub; the leaf is written by the direct-scalar
    // fast path, so the nesting count equals the hub depth reached.
    fn nested_direct_dictionaries(n: usize) -> ObjectHandle {
        let mut handle = ObjectHandle::integer(1);
        for _ in 0..n {
            handle = ObjectHandle::dictionary(vec![(b"/K".to_vec(), handle)]);
        }
        handle
    }

    #[test]
    fn direct_dictionary_cycle_is_rejected_by_the_unparse_hub_family() -> Result<()> {
        let cycle = reciprocal_direct_dictionary_cycle()?;

        let plain = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            ObjectWriterEmission::unparse_object(&cycle, out)
        })
        .expect_err("a direct cycle must not be walked by the plain hub");
        assert!(
            matches!(plain, Error::Unsupported(_)),
            "unexpected error kind: {plain:?}"
        );
        assert!(
            plain
                .to_string()
                .contains("direct object nesting exceeds maximum of"),
            "unexpected message: {plain}"
        );

        let qdf = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            ObjectWriterEmission::unparse_object_qdf(&cycle, out, 0)
        })
        .expect_err("a direct cycle must not be walked by the qdf hub");
        assert!(matches!(qdf, Error::Unsupported(_)));
        assert!(
            qdf.to_string()
                .contains("direct object nesting exceeds maximum of"),
            "unexpected message: {qdf}"
        );

        let mut map = |_: &ObjectHandle| Ok(ObjectRef::new(1, 0));
        let dynamic = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            cycle.unparse_object_with_dynamic_ref_map(out, &mut map, &BTreeSet::new())
        })
        .expect_err("a direct cycle must not be walked by the dynamic ref-map hub");
        assert!(matches!(dynamic, Error::Unsupported(_)));
        assert!(
            dynamic
                .to_string()
                .contains("direct object nesting exceeds maximum of"),
            "unexpected message: {dynamic}"
        );

        // The same callback still serves an ordinary indirect child, so the
        // rejection above is the only behavior the cycle adds to this route.
        let mut mapped = Vec::new();
        let indirect_child = ObjectHandle::array(vec![ObjectHandle::new_indirect_unresolved(
            ObjectRef::new(4, 0),
            -1,
        )]);
        super::super::output::with_buffer_sink(&mut mapped, |out| {
            indirect_child.unparse_object_with_dynamic_ref_map(out, &mut map, &BTreeSet::new())
        })?;
        assert_eq!(mapped, b"[ 1 0 R ]");

        // Every rejected level restores the shared counter on its way out,
        // so a later write starts from zero instead of inheriting the
        // exhausted budget of the refused walk.
        assert_eq!(UNPARSE_WALK_DEPTH.with(std::cell::Cell::get), 0);
        let mut output = Vec::new();
        let deep = nested_direct_dictionaries(crate::parser::MAX_PARSE_DEPTH + 1);
        super::super::output::with_buffer_sink(&mut output, |out| {
            ObjectWriterEmission::unparse_object(&deep, out)
        })?;
        assert_eq!(
            String::from_utf8_lossy(&output).matches("/K").count(),
            crate::parser::MAX_PARSE_DEPTH + 1
        );
        Ok(())
    }

    #[test]
    fn acyclic_direct_nesting_is_bounded_at_the_parser_limit() -> Result<()> {
        let bound = crate::parser::MAX_PARSE_DEPTH;

        let mut output = Vec::new();
        let within_bound = nested_direct_dictionaries(bound + 1);
        super::super::output::with_buffer_sink(&mut output, |out| {
            ObjectWriterEmission::unparse_object(&within_bound, out)
        })?;
        assert_eq!(
            String::from_utf8_lossy(&output).matches("/K").count(),
            bound + 1,
            "every level within the bound must still be written"
        );

        let past_bound = nested_direct_dictionaries(bound + 2);
        let error = super::super::output::with_buffer_sink(&mut Vec::new(), |out| {
            ObjectWriterEmission::unparse_object(&past_bound, out)
        })
        .expect_err("nesting past the bound must be reported");
        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            format!(
                "unsupported PDF feature: writer: direct object nesting exceeds maximum of {bound}"
            )
        );
        Ok(())
    }
}
