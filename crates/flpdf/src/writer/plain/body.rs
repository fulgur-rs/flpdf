//! qpdf correspondence: QPDFWriter.cc plain object-body emission split from planning and xref output.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::writer::object_streams;
use crate::writer::plain::plan::{PlainWritePlan, PlannedIndirectObject};
use crate::writer::plain::xref::{BodyLayout, CompressedLocation};
use crate::writer::WriterOptions;
use crate::writer::{serialize, CompressStreams, ObjectWriterEmission, QPDF_BINARY_MARKER};
use crate::{ObjectHandle, ObjectRef, Pdf};

/// Emit every body placement already chosen by `plan`.
///
/// This stage resolves, rewrites, re-encodes, and serializes planned objects.
/// Numbering, membership, trailer construction, and xref output remain the
/// responsibility of the plan and xref stages.
pub(crate) fn emit_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    plan: &PlainWritePlan,
) -> crate::Result<(Vec<u8>, BodyLayout)> {
    validate_objstm_member_bodies(pdf, plan)?;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{}\n", plan.version).as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);

    let mut layout = BodyLayout::default();
    for planned in &plan.objects {
        match planned {
            PlannedIndirectObject::Source { source, output } => {
                let offset = bytes.len();
                bytes.extend_from_slice(
                    format!("{} {} obj\n", output.number, output.generation).as_bytes(),
                );
                emit_source_from_handle(pdf, options, plan, *source, &mut bytes)?;
                bytes.extend_from_slice(b"\nendobj\n");
                layout
                    .uncompressed
                    .insert(output.number, (output.generation, offset));
                crate::writer::report_progress_event(options)?;
            }
            PlannedIndirectObject::ObjectStream {
                origin,
                output,
                members,
            } => {
                let mut handles = Vec::with_capacity(members.len());
                for member in members {
                    let handle = pdf.get_object_handle(member.source);
                    pdf.resolve(&handle)?;
                    handles.push((member.output, handle));
                }
                let map = |object_ref| {
                    plan.new_for_original(object_ref).ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "plain writer: reference {} {} R absent from renumber map",
                            object_ref.number, object_ref.generation
                        ))
                    })
                };
                let body = object_streams::emit_objstm_body_from_handles_with_writer(
                    &handles,
                    &mut |out, _member_index, _member_ref, handle| {
                        let result = handle.write_object_with_ref_map_and_removed(
                            out,
                            &map,
                            &plan.removed_refs,
                        );
                        if result.is_ok() {
                            crate::writer::report_progress_event(options)?;
                        }
                        result
                    },
                )?;
                let offset = bytes.len();
                bytes.extend_from_slice(
                    format!("{} {} obj\n", output.number, output.generation).as_bytes(),
                );
                let structural_compress = if plan.trailer.structural_filtered {
                    CompressStreams::Yes
                } else {
                    CompressStreams::No
                };
                let extends = match origin {
                    crate::writer::plain::plan::PlannedObjectStreamOrigin::SourceBacked(source) => {
                        let source_handle = pdf.get_object_handle(*source);
                        pdf.resolve(&source_handle)?;
                        if let Some(source_dict) = source_handle.as_stream_dict() {
                            let extends = source_dict.try_get_key(b"/Extends")?;
                            match extends.object_ref() {
                                Some(extends) => Some(
                                    plan.old_to_new.get(&extends).copied().ok_or_else(|| {
                                        crate::Error::Unsupported(format!(
                                            "plain writer: source ObjStm /Extends {} {} R is absent from renumber map",
                                            extends.number, extends.generation
                                        ))
                                    })?,
                                ),
                                _ => None,
                            }
                        } else {
                            // qpdf permits a null or otherwise non-stream source
                            // identity here as a placeholder for a reconstructed
                            // object stream. The rebuilt container still carries
                            // the surviving members, but has no /Extends key.
                            None
                        }
                    }
                    crate::writer::plain::plan::PlannedObjectStreamOrigin::Synthetic => None,
                };
                serialize::write_objstm_stream_with_extends(
                    &mut bytes,
                    &body,
                    structural_compress,
                    options.newline_before_endstream,
                    extends,
                )?; // cov:ignore: error arm requires an in-memory zlib encoder failure
                bytes.extend_from_slice(b"\nendobj\n");
                layout
                    .uncompressed
                    .insert(output.number, (output.generation, offset));
                for (index, member) in members.iter().enumerate() {
                    layout.compressed.insert(
                        member.output.number,
                        CompressedLocation {
                            container: output.number,
                            index: u32::try_from(index).unwrap_or(u32::MAX),
                        },
                    );
                }
            }
        }
    }

    Ok((bytes, layout))
}

fn emit_source_from_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    plan: &PlainWritePlan,
    source: crate::ObjectRef,
    bytes: &mut Vec<u8>,
) -> crate::Result<()> {
    let handle = pdf.get_object_handle(source);
    pdf.resolve(&handle)?;
    let map = |object_ref| {
        plan.new_for_original(object_ref).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer: reference {} {} R absent from renumber map",
                object_ref.number, object_ref.generation
            ))
        })
    };

    if handle.as_stream_dict().is_some() {
        let cached = if let Some(cached) = plan.cached_stream_outputs.get(&source) {
            if cached.fingerprint == crate::writer::plain::plan::stream_cache_fingerprint(&handle)?
            {
                Some(cached)
            } else {
                None
            }
        } else {
            None // cov:ignore: every planned indirect source stream receives a cache entry; structural streams bypass this emitter
        };
        let (dict, data, refiltered) = if let Some(cached) = cached {
            (cached.dict.clone(), cached.data.clone(), cached.refiltered)
        } else {
            canonical_stream_output(&handle, options)?
        };
        dict.write_stream_body_with_ref_map_and_removed(
            bytes,
            refiltered,
            &map,
            &plan.removed_refs,
        )?; // cov:ignore: LLVM attributes the validated success continuation to the call lines above
        serialize::write_stream_payload(bytes, &data, options.newline_before_endstream);
    } else {
        handle.write_object_with_ref_map_and_removed(bytes, &map, &plan.removed_refs)?;
    }
    Ok(())
}

pub(crate) fn canonical_stream_output(
    handle: &ObjectHandle,
    options: &WriterOptions,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool)> {
    let (dict, data, refiltered, _) =
        canonical_stream_output_with_status(handle, options, true, false)?;
    Ok((dict, data, refiltered))
}

pub(crate) fn canonical_stream_output_with_status(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool, bool)> {
    canonical_stream_output_with_rewrite_policy(
        handle,
        options,
        apply_full_rewrite_metadata_policy,
        normalize_content,
    )
}

/// Full-rewrite variant of [`canonical_stream_output`]. The legacy writer
/// applies the same qpdf filter/provider pipeline and the cleartext-metadata
/// policy that belongs to encrypted output. The live handle's source pipe owns
/// recovered stream framing for these non-PCLm routes; the PCLm writer selects
/// its own qpdf `pipeStreamData` length boundary around its queue. The writer
/// must never append scan framing a second time.
pub(crate) fn canonical_stream_output_for_rewrite(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool)> {
    let (dict, data, refiltered, _) =
        canonical_stream_output_for_rewrite_with_status(handle, options, normalize_content)?;
    Ok((dict, data, refiltered))
}

pub(crate) fn canonical_stream_output_for_rewrite_with_status(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool, bool)> {
    canonical_stream_output_with_status(handle, options, true, normalize_content)
}

/// Emit a page or indirect `/Contents` array holder that owns direct stream
/// values. qpdf's stream branch still applies to those nested streams even
/// though the enclosing object is a page dictionary or array, while the
/// ordinary ObjectHandle serializer deliberately emits only a stream's
/// dictionary in a child position. Keep this exceptional framing in the
/// writer consumer rather than changing the generic ObjectHandle contract.
pub(crate) fn emit_content_container_from_handle_with_ref_map(
    container: &ObjectHandle,
    options: &WriterOptions,
    out: &mut Vec<u8>,
    map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<()> {
    let mut write_string = |out: &mut Vec<u8>, value: &[u8]| {
        crate::object::write_string_value(out, value);
        Ok(())
    };
    emit_content_container_from_handle_with_ref_map_and_string_writer(
        container,
        options,
        out,
        map,
        removed_refs,
        &mut write_string,
    )
}

/// Encrypted-string sibling of
/// [`emit_content_container_from_handle_with_ref_map`]. The callback is kept
/// at the same boundary as ObjectHandle's canonical writer methods so direct
/// stream dictionaries do not need a legacy `Object` materialization bridge.
pub(crate) fn emit_content_container_from_handle_with_ref_map_and_string_writer<F>(
    container: &ObjectHandle,
    options: &WriterOptions,
    out: &mut Vec<u8>,
    map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> crate::Result<()>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
{
    let container = if options.content_normalization {
        normalize_content_container(container, options)?
    } else {
        container.clone()
    };
    let mut emitter = ContentEmitter {
        qdf: options.qdf,
        out,
        map,
        removed_refs,
        write_string,
    };
    emitter.emit_value(&container, true, 0)
}

// Stack-safety constants for this module's recursive `ObjectHandle` walkers
// (`ContentEmitter::emit_value`, `has_direct_stream_in_value`,
// `normalize_content_value`), mirroring `object_handle.rs`'s own
// `UNPARSE_STACK_RED_ZONE`/`UNPARSE_STACK_GROWTH_SIZE` (which itself mirrors
// `parser.rs`'s `STACK_RED_ZONE`/`STACK_GROWTH_SIZE`). Kept as a local
// mirror rather than imported cross-module, matching that file's own
// established precedent for this exact constant pair (see its own doc
// comment on why). A container reached here may be built directly through
// the public `ObjectHandle::array`/`dictionary`/`stream` factories (see
// `Pdf::set_object`'s `Object::Stream` lift arm, `reader.rs`), which -- like
// every other `ObjectHandle` tree those factories build -- carries no depth
// bound the way parsed input does.
const CONTENT_EMIT_STACK_RED_ZONE: usize = 32 * 1024;
const CONTENT_EMIT_STACK_GROWTH_SIZE: usize = 1024 * 1024;

/// Replace only direct stream values in a page's `/Contents` value or an
/// indirect array holder. Indirect children retain identity and are never
/// chased: their terminal streams remain ordinary planned objects, exactly as
/// in the shared page-content resolver.
fn normalize_content_container(
    container: &ObjectHandle,
    options: &WriterOptions,
) -> crate::Result<ObjectHandle> {
    container.try_dereference()?;
    if let Some(entries) = container.as_dictionary() {
        let entries = entries
            .into_iter()
            .map(|(key, value)| {
                if key.as_slice() == b"/Contents" {
                    Ok((key, normalize_content_value(&value, options)?))
                } else {
                    Ok((key, value))
                }
            })
            .collect::<crate::Result<Vec<_>>>()?;
        return Ok(ObjectHandle::dictionary(entries));
    }
    if let Some(items) = container.as_array() {
        let items = items
            .into_iter()
            .map(|item| normalize_content_value(&item, options))
            .collect::<crate::Result<Vec<_>>>()?;
        return Ok(ObjectHandle::array(items));
    } // cov:ignore: LLVM does not attribute this successful array normalization continuation
    Ok(container.clone()) // cov:ignore: the pre-scan records only page dictionaries and array holders
}

// The recursion hub for this function's own `Array` arm below -- every
// nested descent funnels back through this same entry point, so wrapping
// here bounds the whole walk the same way `object_handle.rs`'s own
// single-hub recursive walkers do. See `CONTENT_EMIT_STACK_RED_ZONE`'s doc
// for why this needs the same protection those walkers already have.
fn normalize_content_value(
    value: &ObjectHandle,
    options: &WriterOptions,
) -> crate::Result<ObjectHandle> {
    stacker::maybe_grow(
        CONTENT_EMIT_STACK_RED_ZONE,
        CONTENT_EMIT_STACK_GROWTH_SIZE,
        || {
            if value.is_indirect() {
                return Ok(value.clone());
            }
            value.try_dereference()?;
            if value.as_stream_dict().is_some() {
                let (dict, data, _) = canonical_stream_output_for_rewrite(value, options, true)?;
                return Ok(ObjectHandle::stream(dict, Rc::new(data)));
            }
            if let Some(items) = value.as_array() {
                let items = items
                    .into_iter()
                    .map(|item| normalize_content_value(&item, options))
                    .collect::<crate::Result<Vec<_>>>()?;
                return Ok(ObjectHandle::array(items));
            }
            Ok(value.clone())
        },
    )
}

struct ContentEmitter<'a, F>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
{
    qdf: bool,
    out: &'a mut Vec<u8>,
    map: &'a dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
    removed_refs: &'a BTreeSet<ObjectRef>,
    write_string: &'a mut F,
}

impl<F> ContentEmitter<'_, F>
where
    F: FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
{
    // The recursion hub for this impl's `emit_array`/`emit_dictionary`
    // arms below -- every nested descent (including `has_direct_stream_in_value`'s
    // own separate probe walk) funnels back through this same entry point
    // before recursing further, so wrapping here bounds the whole emission
    // walk the same way `object_handle.rs`'s own single-hub recursive
    // walkers do. See `CONTENT_EMIT_STACK_RED_ZONE`'s doc for why this
    // needs the same protection those walkers already have.
    //
    // A direct stream value gets full `stream ... endstream` framing
    // ([`Self::emit_direct_stream`]) wherever it is reached while walking
    // this container -- not only nested under `/Contents` -- because the
    // ordinary per-object `ObjectHandle` serializer this call falls back to
    // otherwise (`write_object_with_ref_map_and_removed_with_string_writer`
    // and its QDF sibling) deliberately inlines only a stream's dictionary
    // at a child position (`unparse_container`'s own doc,
    // `crate::object_handle`). The pre-existing materialized-`Object`
    // writer path this replaced framed every `Object::Stream` node
    // uniformly regardless of nesting position (`Object::write_pdf`), so
    // restoring that framing for any direct stream this walk reaches keeps
    // parity with it rather than reintroducing a silent data-loss gap for
    // a direct stream sibling outside `/Contents`.
    fn emit_value(&mut self, value: &ObjectHandle, root: bool, indent: usize) -> crate::Result<()> {
        stacker::maybe_grow(
            CONTENT_EMIT_STACK_RED_ZONE,
            CONTENT_EMIT_STACK_GROWTH_SIZE,
            || {
                if !root {
                    if let Some(object_ref) = value.object_ref() {
                        if object_ref.number == 0 || self.removed_refs.contains(&object_ref) {
                            self.out.extend_from_slice(b"null");
                        } else {
                            self.out
                                .extend_from_slice((self.map)(object_ref)?.to_string().as_bytes());
                        }
                        return Ok(());
                    }
                }

                value.try_dereference()?;
                if value.as_stream_dict().is_some() {
                    return self.emit_direct_stream(value, indent);
                }

                if root {
                    if value.as_array().is_some() && has_direct_stream_in_value(value)? {
                        let items = value.as_array().ok_or_else(|| {
                            // cov:ignore-start: the handle cannot change between the shape probe and this read
                            crate::Error::Internal(
                                "content array disappeared during emission".into(),
                            )
                            // cov:ignore-end
                        })?; // cov:ignore: the preceding shape probe makes this defensive error unreachable
                        return self.emit_array(&items, indent);
                    }
                    if let Some(entries) = value.as_dictionary() {
                        let has_contents_stream = entries
                            .get(b"/Contents".as_slice())
                            .map(has_direct_stream_in_value)
                            .transpose()?
                            .unwrap_or(false);
                        if has_contents_stream {
                            return self.emit_dictionary(&entries, indent);
                        }
                    }
                } else if has_direct_stream_in_value(value)? {
                    if let Some(items) = value.as_array() {
                        return self.emit_array(&items, indent);
                    }
                    if let Some(entries) = value.as_dictionary() {
                        return self.emit_dictionary(&entries, indent);
                    } // cov:ignore: LLVM does not attribute this successful nested dictionary continuation
                }

                if self.qdf {
                    value.write_object_qdf_with_ref_map_and_removed_with_string_writer(
                        self.out,
                        indent,
                        self.map,
                        self.removed_refs,
                        self.write_string,
                    )
                } else {
                    value.write_object_with_ref_map_and_removed_with_string_writer(
                        self.out,
                        self.map,
                        self.removed_refs,
                        self.write_string,
                    )
                }
            },
        )
    }

    fn emit_array(&mut self, items: &[ObjectHandle], indent: usize) -> crate::Result<()> {
        if self.qdf {
            self.out.extend_from_slice(b"[\n");
            for item in items {
                push_spaces(self.out, indent + 2);
                self.emit_value(item, false, indent + 2)?;
                self.out.push(b'\n');
            }
            push_spaces(self.out, indent);
            self.out.push(b']');
        } else {
            self.out.push(b'[');
            for item in items {
                self.out.push(b' ');
                self.emit_value(item, false, indent)?;
            }
            self.out.extend_from_slice(b" ]");
        }
        Ok(())
    }

    fn emit_dictionary(
        &mut self,
        entries: &BTreeMap<Vec<u8>, ObjectHandle>,
        indent: usize,
    ) -> crate::Result<()> {
        if self.qdf {
            self.out.extend_from_slice(b"<<\n");
        } else {
            self.out.extend_from_slice(b"<<");
        }

        for (key, value) in entries {
            if value.try_is_null()? || is_removed_content_reference(value, self.removed_refs) {
                continue;
            }
            if self.qdf {
                push_spaces(self.out, indent + 2);
            } else {
                self.out.push(b' ');
            }
            write_content_key(self.out, key);
            self.out.push(b' ');
            self.emit_value(value, false, indent + 2)?; // cov:ignore: LLVM does not attribute the successful nested emitter continuation
            if self.qdf {
                self.out.push(b'\n');
            }
        }

        if self.qdf {
            push_spaces(self.out, indent);
            self.out.extend_from_slice(b">>");
        } else {
            self.out.extend_from_slice(b" >>");
        }
        Ok(())
    }

    fn emit_direct_stream(&mut self, stream: &ObjectHandle, indent: usize) -> crate::Result<()> {
        let dict = stream.as_stream_dict().ok_or_else(|| {
            // cov:ignore-start: emit_direct_stream is called only after the stream shape probe
            crate::Error::Internal("direct content stream dictionary is missing".into())
            // cov:ignore-end
        })?; // cov:ignore: the preceding stream shape probe makes this defensive error unreachable
        if self.qdf {
            dict.write_object_qdf_with_ref_map_and_removed_with_string_writer(
                self.out,
                indent,
                self.map,
                self.removed_refs,
                self.write_string,
            )?; // cov:ignore: LLVM does not attribute the successful QDF dictionary continuation
        } else {
            dict.write_object_with_ref_map_and_removed_with_string_writer(
                self.out,
                self.map,
                self.removed_refs,
                self.write_string,
            )?; // cov:ignore: LLVM does not attribute the successful compact dictionary continuation
        }
        self.out.extend_from_slice(b"\nstream\n");
        self.out
            .extend_from_slice(stream.get_raw_stream_data()?.as_ref());
        self.out.extend_from_slice(b"\nendstream");
        Ok(())
    }
}

// The recursion hub for this function's own `Array`/`Dictionary` arms --
// every nested descent funnels back through this same entry point, so
// wrapping here bounds the whole probe walk the same way
// `object_handle.rs`'s own single-hub recursive walkers do. See
// `CONTENT_EMIT_STACK_RED_ZONE`'s doc for why this needs the same
// protection those walkers already have.
fn has_direct_stream_in_value(value: &ObjectHandle) -> crate::Result<bool> {
    stacker::maybe_grow(
        CONTENT_EMIT_STACK_RED_ZONE,
        CONTENT_EMIT_STACK_GROWTH_SIZE,
        || {
            if value.is_indirect() {
                return Ok(false);
            }
            value.try_dereference()?;
            if value.as_stream_dict().is_some() {
                return Ok(true);
            }
            if let Some(items) = value.as_array() {
                for item in items {
                    if has_direct_stream_in_value(&item)? {
                        return Ok(true);
                    }
                }
            } else if let Some(entries) = value.as_dictionary() {
                for (_, child) in entries {
                    if has_direct_stream_in_value(&child)? {
                        return Ok(true);
                    } // cov:ignore: LLVM does not attribute this successful nested dictionary scan continuation
                }
            }
            Ok(false)
        },
    )
}

fn is_removed_content_reference(value: &ObjectHandle, removed_refs: &BTreeSet<ObjectRef>) -> bool {
    value
        .object_ref()
        .is_some_and(|object_ref| removed_refs.contains(&object_ref))
        || value
            .as_reference()
            .is_some_and(|object_ref| removed_refs.contains(&object_ref))
}

fn write_content_key(out: &mut Vec<u8>, key: &[u8]) {
    out.push(b'/');
    let key = key.strip_prefix(b"/").unwrap_or(key);
    crate::object::write_name_escaped(out, key);
}

fn push_spaces(out: &mut Vec<u8>, count: usize) {
    out.resize(out.len().saturating_add(count), b' ');
}

/// Canonical stream output for qpdf's linearized body route.
///
/// `QPDFWriter::writeLinearized` uses the same metadata decision in its
/// `willFilterStream` probe and its final emission (`QPDFWriter.cc:1234-1314`),
/// so planning and writing must share the full-rewrite metadata policy here.
///
/// `normalize_content` must be the caller's own per-stream identity
/// decision (matching qpdf's `m->normalize_content &&
/// m->normalized_streams.count(old_og)` gate, `QPDFWriter.cc:1277`), not a
/// blanket `options.content_normalization` -- normalization applies only to
/// actual page-content streams, never document-wide.
pub(crate) fn canonical_stream_output_for_linearization(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool)> {
    let (dict, data, refiltered, _) =
        canonical_stream_output_for_linearization_with_status(handle, options, normalize_content)?;
    Ok((dict, data, refiltered))
}

pub(crate) fn canonical_stream_output_for_linearization_with_status(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool, bool)> {
    canonical_stream_output_with_status(handle, options, true, normalize_content)
}

/// Return whether the qpdf-shaped stream pipeline will replace the source
/// `/Filter` and `/DecodeParms` entries for a plain rewrite.
///
/// The plain writer must know this before it assigns object numbers: qpdf's
/// `unparseObject` removes those entries before it enqueues dictionary
/// children (`QPDFWriter.cc:1438-1455`); the dictionary child walk is
/// `QPDFWriter.cc:1490-1503`. This helper probes the same pipeline
/// with a discard sink for writer routes that cannot retain the produced
/// buffer. A failed filter probe follows qpdf's retry-to-raw path and
/// therefore returns `false`. The plain planner instead retains the complete
/// output through [`canonical_stream_output_with_status`] so providers are not
/// run again during emission.
pub(crate) fn canonical_stream_will_be_refiltered(
    handle: &ObjectHandle,
    options: &WriterOptions,
) -> crate::Result<bool> {
    canonical_stream_will_be_refiltered_with_policy(handle, options, true, false)
}

/// Probe whether a writer-owned stream will replace its source filter
/// parameters under a specific qpdf writer policy.
///
/// `QPDFWriter::willFilterStream` is called with the state of the writer that
/// will emit the stream. Planning callers must therefore pass the same
/// metadata and content-normalization policy as their emission route; a
/// document-wide default is not equivalent when linearization and full
/// rewrite have different metadata handling.
pub(crate) fn canonical_stream_will_be_refiltered_with_policy(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<bool> {
    // Token filters are stateful qpdf ValueSetter-style consumers. The plain
    // planner caches their complete output before walking references; callers
    // that cannot retain that output must leave the stream edge intact rather
    // than consuming the filter and running it again during emission.
    if handle.is_data_modified() {
        return Ok(false);
    }
    let Some((encode_flags, decode_level, _normalized_content)) = canonical_stream_filter_plan(
        handle,
        options,
        apply_full_rewrite_metadata_policy,
        normalize_content,
    )?
    else {
        return Ok(false);
    };

    for attempt in 1..=2 {
        let mut discard = crate::pipeline::Discard;
        let mut filtering_attempted = false;
        let (attempt_encode_flags, attempt_decode_level) = if attempt == 1 {
            (encode_flags, decode_level)
        } else {
            (0, crate::writer::DecodeLevel::None)
        };
        let success = handle.pipe_stream_data(
            &mut discard,
            &mut filtering_attempted,
            attempt_encode_flags,
            attempt_decode_level,
            true,
            attempt == 1,
        )?;
        if success || attempt == 2 {
            return Ok(filtering_attempted && success);
        }
    }

    unreachable!("the two-attempt stream filter probe always returns") // cov:ignore: the bounded two-attempt loop returns from both attempts
}

fn canonical_stream_output_with_rewrite_policy(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, bool, bool)> {
    let stream_dict = handle
        .as_stream_dict()
        .ok_or_else(|| crate::Error::Internal("canonical stream dictionary is missing".into()))?;
    // The CLI's page-content normalizer records its completed transform on
    // the live handle. Generic `replaceStreamData` calls remain eligible for
    // QDF normalization, because replacement bytes alone do not establish
    // that this particular consumer has already normalized them. The filter
    // plan still treats the marker as an effective normalization request so
    // those already-raw bytes are not recompressed.
    // QPDFWriter.cc:1251-1278 gives cleartext /Type /Metadata streams their
    // own policy: decode fully and emit without a filter, even when the global
    // writer policy would preserve or compress a lone-Flate source. The plain
    // route is unencrypted, so this exception always applies here.
    let is_metadata_stream = apply_full_rewrite_metadata_policy
        && stream_dict.try_is_dictionary_of_type(b"Metadata", b"")?
        && options
            .encrypt
            .as_ref()
            .is_none_or(|params| !params.encrypt_metadata)
        && options
            .copy_encryption
            .as_ref()
            .is_none_or(|source| !crate::writer::copy_encryption_encrypts_metadata(source));
    // qpdf's `willFilterStream` treats the cleartext-metadata and
    // content-normalization branches as mutually exclusive: an `if
    // (is_metadata) ... else if (normalize_content) ...` chain
    // (`QPDFWriter.cc:1274-1284`), so `normalize` never becomes true once
    // `is_metadata` wins it. Metadata is not page content, so it must never
    // receive content-token normalization, mirroring the same guard
    // `reencode_stream_for_compress` already applies in `writer.rs`.
    let normalize_content = normalize_content && !is_metadata_stream;
    let policy = if is_metadata_stream {
        Some(CompressStreams::No)
    } else {
        crate::writer::effective_stream_policy(options)
    };
    let source_for_pipe = handle.clone();

    // QPDFWriter::willFilterStream starts with `isDataModified()` before it
    // considers the user compression policy (`QPDFWriter.cc:1234-1245`). A
    // token-filtered stream must therefore take the pipe path even under
    // Preserve mode; only an unmodified stream may be emitted verbatim.
    let filter_plan = canonical_stream_filter_plan(
        handle,
        options,
        apply_full_rewrite_metadata_policy,
        normalize_content,
    )?; // cov:ignore: canonical stream policy validation is exercised by the body tests; llvm-cov attributes this continuation to the defensive error path
    let (data, filtering_attempted, normalized_content) =
        if let Some((encode_flags, decode_level, normalized_content)) = filter_plan {
            let mut attempt = 1_u8;
            let (data, filtering_attempted) = loop {
                let mut buffer =
                    crate::pipeline::buffer::Buffer::new("canonical writer stream", None);
                let mut filtering_attempted = false;
                let (attempt_encode_flags, attempt_decode_level) = if attempt == 1 {
                    (encode_flags, decode_level)
                } else {
                    (0, crate::writer::DecodeLevel::None)
                };
                let success = source_for_pipe.pipe_stream_data(
                    &mut buffer,
                    &mut filtering_attempted,
                    attempt_encode_flags,
                    attempt_decode_level,
                    false,
                    attempt == 1,
                )?; // cov:ignore: filter-pipeline failures are covered at the pipeline boundary, not by this validated emitter

                if success || attempt == 2 {
                    // QPDFWriter retries a failed filter pipeline against a
                    // fresh raw pipe (`QPDFWriter.cc:1287-1314`). The second
                    // attempt's buffer is authoritative even when the
                    // provider reports that no filtering branch was used.
                    break (
                        buffer.take_buffer()?.to_vec(),
                        filtering_attempted && success,
                    );
                }
                attempt = 2;
            };
            (data, filtering_attempted, normalized_content)
        } else {
            (
                source_for_pipe.get_raw_stream_data()?.as_ref().clone(),
                false,
                false,
            )
        };
    let mut entries = stream_dict.try_as_dictionary()?.unwrap_or_default();
    entries.remove(b"/Length".as_slice());
    if !filtering_attempted {
        remove_crypt_filter_for_unfiltered_stream(&mut entries)?;
    }
    if filtering_attempted {
        entries.retain(|key, _| {
            !matches!(
                key.as_slice(),
                b"/Filter" | b"/DecodeParms" | b"/F" | b"/FFilter" | b"/FDecodeParms"
            )
        });
        // qpdf's normalization branch wins over the ordinary compression
        // branch (`QPDFWriter.cc:1279-1284`): normalized page content is
        // emitted decoded, even when compress_streams is enabled.
        if matches!(policy, Some(CompressStreams::Yes)) && !normalized_content {
            entries.insert(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            );
        }
    }
    entries.insert(
        b"/Length".to_vec(),
        ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
    );
    let dict = ObjectHandle::dictionary(entries.into_iter().collect());
    let refiltered =
        filtering_attempted && matches!(policy, Some(CompressStreams::Yes)) && !normalized_content;
    Ok((dict, data, refiltered, filtering_attempted))
}

fn canonical_stream_filter_plan(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<Option<(u32, crate::writer::DecodeLevel, bool)>> {
    let stream_dict = handle
        .as_stream_dict()
        .ok_or_else(|| crate::Error::Internal("canonical stream dictionary is missing".into()))?;
    // The CLI may have already normalized and replaced this stream's bytes.
    // Keep that state as an effective normalization request so qpdf's
    // normalization branch still suppresses compression, while avoiding a
    // second tokenizer pass on the same bytes.
    let normalization_applied = normalize_content && handle.content_normalization_applied();
    let normalize_content = normalize_content && !normalization_applied;
    let source_has_lone_flate = canonical_is_lone_flate(&stream_dict)?;
    let is_metadata_stream = apply_full_rewrite_metadata_policy
        && stream_dict.try_is_dictionary_of_type(b"Metadata", b"")?
        && options
            .encrypt
            .as_ref()
            .is_none_or(|params| !params.encrypt_metadata)
        && options
            .copy_encryption
            .as_ref()
            .is_none_or(|source| !crate::writer::copy_encryption_encrypts_metadata(source));
    let normalize_content = normalize_content && !is_metadata_stream;
    let normalized_content = (normalize_content || normalization_applied) && !is_metadata_stream;
    let policy = if is_metadata_stream {
        Some(CompressStreams::No)
    } else {
        crate::writer::effective_stream_policy(options)
    };
    let decode_level = if is_metadata_stream {
        crate::writer::DecodeLevel::All
    } else {
        options.decode_level
    };
    let preserve_lone_flate = matches!(policy, Some(CompressStreams::Yes))
        && source_has_lone_flate
        && !handle.is_data_modified()
        && !options.recompress_flate
        && !normalized_content
        && !stream_dict.try_has_key(b"/F")?;
    if !handle.is_data_modified() && (policy.is_none() || preserve_lone_flate) {
        return Ok(None);
    }

    // qpdf's `normalize_content` branch is an `else if` before the ordinary
    // `compress_streams` branch (`QPDFWriter.cc:1279-1284`), so normalization
    // must not also request Flate re-encoding.
    let mut encode_flags = if matches!(policy, Some(CompressStreams::Yes)) && !normalized_content {
        crate::object_handle::STREAM_ENCODE_COMPRESS
    } else {
        0
    };
    if normalize_content {
        encode_flags |= crate::object_handle::STREAM_ENCODE_NORMALIZE;
    }
    Ok(Some((encode_flags, decode_level, normalized_content)))
}

/// Mirror `QPDFWriter::unparseObject`'s unfiltered-stream cleanup
/// (`libqpdf/QPDFWriter.cc:1446-1478`): source encryption is applied by the
/// resolver pipe, so the writer never carries an explicit `/Crypt` stage into
/// a rewritten stream dictionary. A direct `/Crypt` removes both filter keys;
/// an array removes the first `/Crypt` item and its paired `/DecodeParms` slot,
/// preserving the remaining filter representation byte-for-byte.
fn remove_crypt_filter_for_unfiltered_stream(
    entries: &mut std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
) -> crate::Result<()> {
    let Some(filter) = entries.get(b"/Filter".as_slice()).cloned() else {
        return Ok(());
    };

    if filter.try_is_name_and_equals(b"Crypt")? {
        entries.remove(b"/Filter".as_slice());
        entries.remove(b"/DecodeParms".as_slice());
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

    let mut remaining_filters = filters;
    remaining_filters.remove(crypt_index);
    entries.insert(b"/Filter".to_vec(), ObjectHandle::array(remaining_filters));

    let Some(decode_parms) = entries.get(b"/DecodeParms".as_slice()).cloned() else {
        return Ok(());
    };
    let Some(mut decode_items) = decode_parms.try_as_array()? else {
        return Ok(());
    };
    if decode_items.len() > crypt_index {
        decode_items.remove(crypt_index);
        entries.insert(b"/DecodeParms".to_vec(), ObjectHandle::array(decode_items));
    }
    Ok(())
}

fn canonical_is_lone_flate(dict: &ObjectHandle) -> crate::Result<bool> {
    let filter = dict.try_get_key(b"/Filter")?;
    if filter.try_is_null()? {
        return Ok(false);
    }
    if filter.try_is_name_and_equals(b"FlateDecode")? || filter.try_is_name_and_equals(b"Fl")? {
        return Ok(true);
    }
    Ok(false)
}

fn validate_objstm_member_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    plan: &PlainWritePlan,
) -> crate::Result<()> {
    if !plan
        .objects
        .iter()
        .any(|object| matches!(object, PlannedIndirectObject::ObjectStream { .. }))
    {
        return Ok(());
    }

    let context = object_streams::eligibility_context(pdf)?;
    for planned in &plan.objects {
        let PlannedIndirectObject::ObjectStream { members, .. } = planned else {
            continue;
        };
        for member in members {
            let member_handle = pdf.get_object_handle(member.source);
            pdf.resolve(&member_handle)?;
            let is_signature = object_streams::is_qpdf_signature_dict(pdf, &member_handle)?;
            let violation = planned_member_body_violation(
                member.source,
                member.output,
                &member_handle,
                &context,
            )? // cov:ignore: trailing `)?` on a multi-line validation call — llvm-cov attributes the validated continuation to the Err path
            .or(is_signature.then_some("signature dictionary"));
            if let Some(kind) = violation {
                return Err(crate::Error::Unsupported(format!(
                    "plain writer body invariant: source {} planned as ObjStm member {} \
                     resolves to forbidden {kind}",
                    member.source, member.output
                )));
            }
        }
    }
    Ok(())
}

fn planned_member_body_violation(
    source: crate::ObjectRef,
    output: crate::ObjectRef,
    object: &ObjectHandle,
    context: &object_streams::EligibilityContext,
) -> crate::Result<Option<&'static str>> {
    if output.generation != 0 {
        return Ok(Some("nonzero output generation"));
    }
    if object.as_stream_dict().is_some() {
        return Ok(Some("stream body"));
    }
    if object.try_is_dictionary_of_type(b"XRef", b"")? {
        return Ok(Some("/Type /XRef dictionary"));
    }
    if object.try_is_dictionary_of_type(b"ObjStm", b"")? {
        return Ok(Some("/Type /ObjStm dictionary"));
    }
    if context.encryption_ref == Some(source) {
        return Ok(Some("encryption dictionary"));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::plain::plan::PlannedIndirectObject;
    use crate::{Dictionary, NewlineBeforeEndstream, Object, ObjectRef, ObjectStreamMode, Stream};
    use std::cell::RefCell;
    use std::io::{Cursor, Read, Seek};

    fn resolved_object<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        object_ref: ObjectRef,
    ) -> crate::Result<Object> {
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle)?;
        handle.materialize()
    }

    #[test]
    fn unfiltered_crypt_cleanup_handles_missing_and_scalar_decode_parms() {
        for decode_parms in [None, Some(ObjectHandle::integer(7))] {
            let mut entries = std::collections::BTreeMap::new();
            entries.insert(
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"Crypt".to_vec()),
                ]),
            );
            if let Some(decode_parms) = decode_parms {
                entries.insert(b"/DecodeParms".to_vec(), decode_parms);
            }

            remove_crypt_filter_for_unfiltered_stream(&mut entries).unwrap();

            assert_eq!(
                entries
                    .get(b"/Filter".as_slice())
                    .and_then(|filter| filter.as_array())
                    .map(|filters| filters.len()),
                Some(1)
            );
        }
    }

    #[test]
    fn disable_emission_records_every_planned_source_offset() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let (_, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();
        assert!(layout.compressed.is_empty());
        assert_eq!(
            layout.uncompressed.len(),
            plan.objects
                .iter()
                .filter(|object| matches!(object, PlannedIndirectObject::Source { .. }))
                .count()
        );
    }

    #[test]
    fn object_stream_emission_records_container_and_member_locations() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        for compress_streams in [CompressStreams::Yes, CompressStreams::No] {
            let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
            let options = WriterOptions {
                object_streams: ObjectStreamMode::Generate,
                compress_streams,
                static_id: true,
                newline_before_endstream: NewlineBeforeEndstream::Never,
                ..WriterOptions::default()
            };
            let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
            let (bytes, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();
            layout.validate().unwrap();

            let mut planned_members = 0;
            for object in &plan.objects {
                match object {
                    PlannedIndirectObject::Source { output, .. } => {
                        let (_, offset) = layout.uncompressed[&output.number];
                        assert!(bytes[offset..].starts_with(
                            format!("{} {} obj\n", output.number, output.generation).as_bytes()
                        ));
                    }
                    PlannedIndirectObject::ObjectStream {
                        output, members, ..
                    } => {
                        let (_, offset) = layout.uncompressed[&output.number];
                        assert!(bytes[offset..].starts_with(
                            format!("{} {} obj\n", output.number, output.generation).as_bytes()
                        ));
                        for (index, member) in members.iter().enumerate() {
                            assert_eq!(
                                layout.compressed[&member.output.number],
                                CompressedLocation {
                                    container: output.number,
                                    index: u32::try_from(index).unwrap(),
                                }
                            );
                        }
                        planned_members += members.len();
                    }
                }
            }
            assert!(planned_members > 0);
            assert_eq!(layout.compressed.len(), planned_members);
        }
    }

    #[test]
    fn source_emission_propagates_reference_rewrite_failure() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.old_to_new.remove(&crate::ObjectRef::new(2, 0));

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("reference 2 0 R absent from renumber map")));
    }

    #[test]
    fn canonical_emission_reuses_a_recovered_stream_eol() {
        let fixture =
            include_bytes!("../../../../../tests/fixtures/compat/null-length-framing-matrix.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        resolved_object(&mut pdf, ObjectRef::new(5, 0))
            .expect("canonical resolution records the recovered framing EOL");
        let stream_handle = pdf.get_object_handle(ObjectRef::new(5, 0));
        assert_eq!(
            pdf.canonical_recovered_stream_eol(ObjectRef::new(5, 0), &stream_handle)
                .expect("canonical recovered EOL"),
            Some(&b"\n"[..])
        );
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            stream_data: Some(crate::StreamDataMode::Preserve),
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let (bytes, _) = emit_bodies(&mut pdf, &options, &plan).unwrap();
        assert!(bytes
            .windows(b"missing-lf".len())
            .any(|window| window == b"missing-lf"));
    }

    #[test]
    fn canonical_stream_output_does_not_duplicate_a_recovered_stream_eol() {
        let fixture =
            include_bytes!("../../../../../tests/fixtures/compat/null-length-framing-matrix.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        resolved_object(&mut pdf, ObjectRef::new(5, 0))
            .expect("canonical resolution records the recovered framing EOL");
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            stream_data: Some(crate::StreamDataMode::Preserve),
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };

        let handle = pdf.get_object_handle(ObjectRef::new(5, 0));
        pdf.resolve(&handle).unwrap();
        let (_, data, _) = canonical_stream_output(&handle, &options).unwrap();

        assert_eq!(data, b"missing-lf\n");
    }

    #[test]
    fn canonical_stream_output_decodes_metadata_even_under_compress_policy() {
        let mut filter_dict = Dictionary::new();
        filter_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded =
            crate::filters::test_dictionary_api::encode_stream_data(&filter_dict, b"metadata")
                .expect("metadata payload must encode");
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"Type".to_vec(), ObjectHandle::name(b"Metadata".to_vec())),
                (
                    b"Filter".to_vec(),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ),
                (
                    b"Length".to_vec(),
                    ObjectHandle::integer(encoded.len() as i64),
                ),
            ]),
            Rc::new(encoded),
        );

        let options = WriterOptions::default();
        let (dict, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(data, b"metadata");
        assert!(!refiltered);
        assert!(!dict.try_has_key(b"/Filter").unwrap());
    }

    #[test]
    fn canonical_emission_turns_a_reference_to_a_removed_object_into_null() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let root = pdf.root_ref().unwrap();
        let removed = ObjectRef::new(100, 0);
        let stream_ref = ObjectRef::new(101, 0);
        pdf.set_object(removed, Object::Integer(7));
        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Length", Object::Integer(3));
        stream_dict.insert("StreamRemovedDirect", Object::Reference(removed));
        pdf.set_object(
            stream_ref,
            Object::Stream(Stream {
                dict: stream_dict,
                data: b"abc".to_vec(),
            }),
        );
        let mut catalog = resolved_object(&mut pdf, root)
            .unwrap()
            .into_dict()
            .unwrap();
        catalog.insert("RemovedDirect", Object::Reference(removed));
        catalog.insert(
            "RemovedArray",
            Object::Array(vec![Object::Reference(removed)]),
        );
        catalog.insert("RemovedStream", Object::Reference(stream_ref));
        pdf.set_object(root, Object::Dictionary(catalog));
        pdf.delete_object(removed);

        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let (bytes, _) = emit_bodies(&mut pdf, &options, &plan).unwrap();

        assert!(bytes
            .windows(b"/RemovedArray [ null ]".len())
            .any(|window| window == b"/RemovedArray [ null ]"));
        assert!(!bytes
            .windows(b"/RemovedDirect".len())
            .any(|window| window == b"/RemovedDirect"));
        assert!(!bytes
            .windows(b"/StreamRemovedDirect".len())
            .any(|window| window == b"/StreamRemovedDirect"));
    }

    #[test]
    fn canonical_stream_output_retries_with_the_raw_payload_after_a_source_decode_failure() {
        let mut bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let bodies: [(u32, &[u8]); 4] = [
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 10 10] >>",
            ),
            (
                4,
                b"<< /Length 3 /Filter /FlateDecode >>\nstream\nabc\nendstream",
            ),
        ];
        let mut offsets = [0_usize; 5];
        for (number, body) in bodies {
            offsets[number as usize] = bytes.len();
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );

        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let stream = pdf.get_object_handle(ObjectRef::new(4, 0));
        pdf.resolve(&stream).unwrap();
        let options = WriterOptions {
            recompress_flate: true,
            ..WriterOptions::default()
        };

        let (dict, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(data, b"abc");
        assert!(!refiltered);
        assert!(dict
            .get_key(b"/Filter")
            .try_is_name_and_equals(b"FlateDecode")
            .unwrap());
    }

    #[test]
    fn refilter_probe_retries_an_invalid_source_as_raw() {
        let fixture =
            include_bytes!("../../../../../tests/fixtures/test_driver/stream_flate_error.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let stream = pdf.get_object_handle(ObjectRef::new(6, 0));
        pdf.resolve(&stream).unwrap();
        let options = WriterOptions {
            recompress_flate: true,
            ..WriterOptions::default()
        };

        assert!(!canonical_stream_will_be_refiltered(&stream, &options).unwrap());
    }

    #[test]
    fn refilter_probe_rejects_a_non_stream_handle() {
        let error = canonical_stream_will_be_refiltered_with_policy(
            &ObjectHandle::integer(1),
            &WriterOptions::default(),
            true,
            false,
        )
        .expect_err("refilter probing requires a stream handle");
        assert!(
            matches!(error, crate::Error::Internal(message) if message.contains("stream dictionary"))
        );
    }

    #[test]
    fn refilter_probe_does_not_consume_a_stateful_token_filter() {
        struct ForwardTokenFilter;

        impl crate::token_filter::TokenFilter for ForwardTokenFilter {
            fn handle_token(
                &mut self,
                token: &crate::tokenizer::Token,
                output: &mut crate::token_filter::TokenFilterOutput<'_>,
            ) -> crate::pipeline::PipelineResult<()> {
                output.write_token(token)
            }
        }

        let mut filter = ForwardTokenFilter;
        let token = crate::tokenizer::Token::new(crate::tokenizer::TokenType::Word, b"q".to_vec());
        let mut output = crate::token_filter::TokenFilterOutput::new(None);
        crate::token_filter::TokenFilter::handle_token(&mut filter, &token, &mut output).unwrap();

        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(1))]),
            Rc::new(b"q".to_vec()),
        );
        stream
            .add_token_filter(Rc::new(RefCell::new(filter)))
            .unwrap();

        assert!(!canonical_stream_will_be_refiltered(&stream, &WriterOptions::default()).unwrap());
    }

    #[test]
    fn canonical_stream_output_retries_provider_with_qpdf_attempt_flags() {
        let pdf = Pdf::empty().unwrap();
        let stream = pdf.new_stream().unwrap();
        let attempts = Rc::new(RefCell::new(Vec::new()));
        let attempts_in_callback = Rc::clone(&attempts);

        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    attempts_in_callback
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    pipeline
                        .write(if will_retry {
                            b"filtered provider bytes"
                        } else {
                            b"raw provider bytes"
                        })
                        .map_err(crate::Error::from)?;
                    pipeline.finish().map_err(crate::Error::from)?;
                    Ok(!will_retry)
                },
                None,
                None,
            )
            .unwrap();

        let options = WriterOptions {
            compress_streams: CompressStreams::Yes,
            ..WriterOptions::default()
        };
        let (_, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(*attempts.borrow(), vec![(false, true), (false, false)]);
        assert_eq!(data, b"raw provider bytes");
        assert!(!refiltered);
    }

    #[test]
    fn body_emission_retries_a_provider_with_qpdf_attempt_flags() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            compress_streams: CompressStreams::Yes,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let source = plan
            .objects
            .iter()
            .find_map(|planned| {
                let PlannedIndirectObject::Source { source, .. } = planned else {
                    return None; // cov:ignore: ObjectStreamMode::Disable plans contain only source placements; defensive future-plan arm
                };
                let handle = pdf.get_object_handle(*source);
                pdf.resolve(&handle).ok()?;
                handle.as_stream_dict().map(|_| *source)
            })
            .expect("three-page fixture must contain a source stream");
        let stream = pdf.get_object_handle(source);
        let attempts = Rc::new(RefCell::new(Vec::new()));
        let attempts_in_callback = Rc::clone(&attempts);
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    attempts_in_callback
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    pipeline
                        .write(if will_retry {
                            b"filtered provider bytes"
                        } else {
                            b"raw provider bytes"
                        })
                        .map_err(crate::Error::from)?;
                    pipeline.finish().map_err(crate::Error::from)?;
                    Ok(!will_retry)
                },
                Some(ObjectHandle::null()),
                Some(ObjectHandle::null()),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&stream).unwrap();

        let (bytes, _) = emit_bodies(&mut pdf, &options, &plan).unwrap();

        assert_eq!(*attempts.borrow(), vec![(false, true), (false, false)]);
        assert!(bytes
            .windows(b"raw provider bytes".len())
            .any(|window| window == b"raw provider bytes"));
        assert!(!bytes
            .windows(b"filtered provider bytes".len())
            .any(|window| window == b"filtered provider bytes"));
    }

    #[test]
    fn body_emission_recomputes_cached_output_after_provider_replacement() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            compress_streams: CompressStreams::No,
            static_id: true,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let source = pdf
            .object_refs()
            .into_iter()
            .find_map(|source| {
                let handle = pdf.get_object_handle(source);
                pdf.resolve(&handle).ok()?;
                handle.as_stream_dict().map(|_| source)
            })
            .expect("three-page fixture must contain a source stream");
        let stream = pdf.get_object_handle(source);
        pdf.resolve(&stream).unwrap();
        stream
            .replace_stream_data_with_callback(
                |pipeline| {
                    pipeline.write(b"provider A").map_err(crate::Error::from)?;
                    pipeline.finish().map_err(crate::Error::from)
                },
                Some(ObjectHandle::null()),
                Some(ObjectHandle::null()),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&stream).unwrap();

        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        stream
            .replace_stream_data_with_callback(
                |pipeline| {
                    pipeline.write(b"provider B").map_err(crate::Error::from)?;
                    pipeline.finish().map_err(crate::Error::from)
                },
                Some(ObjectHandle::null()),
                Some(ObjectHandle::null()),
            )
            .unwrap();
        pdf.mark_object_handle_dirty(&stream).unwrap();

        let (bytes, _) = emit_bodies(&mut pdf, &options, &plan).unwrap();
        assert!(bytes
            .windows(b"provider B".len())
            .any(|window| window == b"provider B"));
        assert!(!bytes
            .windows(b"provider A".len())
            .any(|window| window == b"provider A"));
    }

    #[test]
    fn canonical_stream_output_recognizes_a_lone_flate_abbreviation() {
        let dict = ObjectHandle::dictionary(vec![
            (b"Filter".to_vec(), ObjectHandle::name(b"Fl".to_vec())),
            (b"Length".to_vec(), ObjectHandle::integer(3)),
        ]);

        assert!(canonical_is_lone_flate(&dict).unwrap());

        let stream = ObjectHandle::stream(dict, Rc::new(b"abc".to_vec()));
        let options = WriterOptions {
            compress_streams: CompressStreams::Yes,
            ..WriterOptions::default()
        };

        let (_, data, refiltered) = canonical_stream_output(&stream, &options).unwrap();

        assert_eq!(data, b"abc");
        assert!(!refiltered);
    }

    #[test]
    fn disable_emission_serializes_reachable_source_objstm_container() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let root = pdf.root_ref().unwrap();
        let mut catalog = resolved_object(&mut pdf, root)
            .unwrap()
            .into_dict()
            .unwrap();
        let source_objstm = ObjectRef::new(1, 0);
        catalog.insert("ReachableStructural", Object::Reference(source_objstm));
        pdf.set_object(root, Object::Dictionary(catalog));
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let output_objstm = plan.new_for_original(source_objstm).unwrap();

        let (bytes, layout) = emit_bodies(&mut pdf, &options, &plan).unwrap();

        assert!(layout.uncompressed.contains_key(&output_objstm.number));
        assert!(bytes
            .windows(b"/Type /ObjStm".len())
            .any(|window| window == b"/Type /ObjStm"));
    }

    #[test]
    fn object_stream_emission_propagates_reference_rewrite_failure() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.old_to_new.remove(&crate::ObjectRef::new(2, 0));

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("reference 2 0 R absent from renumber map")));
    }

    #[test]
    fn source_objstm_extends_must_be_present_in_the_renumber_map() {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page-objstm.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Preserve,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let mut candidates = vec![PlannedIndirectObject::Source {
            source: ObjectRef::new(0, 0),
            output: ObjectRef::new(0, 0),
        }];
        candidates.extend(plan.objects.iter().cloned());
        let container_source = candidates
            .iter()
            .find_map(|object| match object {
                PlannedIndirectObject::ObjectStream {
                    origin:
                        crate::writer::plain::plan::PlannedObjectStreamOrigin::SourceBacked(source),
                    ..
                } => Some(*source),
                PlannedIndirectObject::Source { .. }
                | PlannedIndirectObject::ObjectStream {
                    origin: crate::writer::plain::plan::PlannedObjectStreamOrigin::Synthetic,
                    ..
                } => None,
            })
            .expect("preserve plan must retain a source-backed ObjStm");

        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        dict.insert("N", Object::Integer(0));
        dict.insert("First", Object::Integer(0));
        dict.insert("Length", Object::Integer(0));
        dict.insert("Extends", Object::Reference(ObjectRef::new(99_999, 0)));
        pdf.set_object(
            container_source,
            Object::Stream(Stream {
                dict,
                data: Vec::new(),
            }),
        );

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();
        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("/Extends 99999 0 R is absent from renumber map")));
    }

    fn assert_invalid_planned_member_is_rejected(invalid: Object, expected_kind: &str) {
        let fixture = include_bytes!("../../../../../tests/fixtures/compat/three-page.pdf");
        let mut pdf = Pdf::open(Cursor::new(&fixture[..])).unwrap();
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let members: Vec<_> = plan
            .objects
            .iter()
            .filter_map(|object| match object {
                PlannedIndirectObject::ObjectStream { members, .. } => Some(members),
                PlannedIndirectObject::Source { .. } => None,
            })
            .flatten()
            .cloned()
            .collect();
        let member = members
            .into_iter()
            .find(|member| Some(member.source) != pdf.root_ref())
            .unwrap();
        pdf.set_object(member.source, invalid);

        let error = emit_bodies(&mut pdf, &options, &plan).unwrap_err();

        assert!(matches!(error, crate::Error::Unsupported(ref message)
            if message.contains("plain writer body invariant")
                && message.contains(&member.source.to_string())
                && message.contains(expected_kind)));
    }

    #[test]
    fn invalid_planned_objstm_stream_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::Integer(0));
        assert_invalid_planned_member_is_rejected(
            Object::Stream(Stream {
                dict,
                data: Vec::new(),
            }),
            "stream",
        );
    }

    #[test]
    fn invalid_planned_objstm_xref_dictionary_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"XRef".to_vec()));
        assert_invalid_planned_member_is_rejected(Object::Dictionary(dict), "/Type /XRef");
    }

    #[test]
    fn invalid_planned_nested_objstm_dictionary_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        assert_invalid_planned_member_is_rejected(Object::Dictionary(dict), "/Type /ObjStm");
    }

    #[test]
    fn invalid_planned_signature_dictionary_member_is_rejected() {
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"Sig".to_vec()));
        dict.insert("ByteRange", Object::Array(Vec::new()));
        dict.insert("Contents", Object::String(vec![0]));
        assert_invalid_planned_member_is_rejected(Object::Dictionary(dict), "signature");
    }

    #[test]
    fn planned_member_identity_invariants_are_classified() {
        let source = crate::ObjectRef::new(7, 0);
        let object = ObjectHandle::null();
        let ordinary = object_streams::EligibilityContext {
            encryption_ref: None,
        };
        assert_eq!(
            planned_member_body_violation(source, crate::ObjectRef::new(2, 1), &object, &ordinary,)
                .unwrap(),
            Some("nonzero output generation")
        );

        let encrypted = object_streams::EligibilityContext {
            encryption_ref: Some(source),
        };
        assert_eq!(
            planned_member_body_violation(source, crate::ObjectRef::new(2, 0), &object, &encrypted)
                .unwrap(),
            Some("encryption dictionary")
        );
    }

    // A tree built through the public `ObjectHandle::array`/`dictionary`
    // factories carries no depth bound the way parsed input does (see
    // `object_handle.rs`'s own `UNPARSE_STACK_RED_ZONE` doc), so
    // `has_direct_stream_in_value` must survive nesting deep enough to
    // overflow an unprotected default thread stack. Mirrors
    // `object_handle::mutation_tests::deep_containment_traversals_probe`'s
    // subprocess-probe shape: the outer test spawns this binary again,
    // targeting only the ignored probe, and checks the child exited cleanly
    // rather than aborting from a stack overflow.
    #[test]
    fn deeply_nested_direct_contents_do_not_overflow_the_stack() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "writer::plain::body::tests::deeply_nested_direct_contents_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("FLPDF_DEEP_CONTENT_SCAN_PROBE", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            output.status.success(),
            "deep content-scan probe failed: status={} stderr={}",
            output.status,
            stderr
        );
    }

    #[test]
    #[ignore = "subprocess-only stack-overflow regression probe"]
    fn deeply_nested_direct_contents_probe() {
        assert_eq!(
            std::env::var_os("FLPDF_DEEP_CONTENT_SCAN_PROBE").as_deref(),
            Some(std::ffi::OsStr::new("1"))
        );

        let leaf = ObjectHandle::integer(1);
        let mut nested = leaf;
        for _ in 0..100_000 {
            nested = ObjectHandle::array(vec![nested]);
        }

        let has_stream = has_direct_stream_in_value(&nested).unwrap();
        assert!(!has_stream);

        // This user-constructed value is intentionally deeper than Rust's
        // recursive Rc drop can safely release. Keep this probe scoped to
        // `has_direct_stream_in_value` under test, matching
        // `deep_containment_traversals_probe`'s own `mem::forget` rationale.
        std::mem::forget(nested);
    }
}
