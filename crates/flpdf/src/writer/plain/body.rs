//! qpdf correspondence: QPDFWriter.cc plain object-body emission split from planning and xref output.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::writer::object_streams;
use crate::writer::plain::plan::{
    PlainWritePlan, PlannedIndirectObject, PlannedMember, PlannedObjectStreamOrigin,
};
use crate::writer::plain::xref::{BodyLayout, CompressedLocation};
use crate::writer::write_object::WriteObject;
use crate::writer::WriterOptions;
use crate::writer::{
    serialize, CompressStreams, ObjectWriterEmission, StreamDictionaryOptions, QPDF_BINARY_MARKER,
};
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
    let mut emitter = PlainObjectEmitter {
        pdf,
        options,
        plan,
        bytes: &mut bytes,
        layout: &mut layout,
        lengths: BTreeMap::new(),
        object_stream_to_objects: plan
            .objects
            .iter()
            .filter_map(|planned| match planned {
                PlannedIndirectObject::ObjectStream {
                    origin: origin @ PlannedObjectStreamOrigin::SourceBacked(source),
                    output,
                    members,
                } => Some((source.number, (origin, *output, members.as_slice()))),
                _ => None,
            })
            .collect(),
        encryption: crate::writer::encryption_state::WriterEncryptionState::new(
            false,
            Vec::new(),
            false,
            0,
            0,
        ),
    };
    for planned in &plan.objects {
        match planned {
            PlannedIndirectObject::Source { source, .. } => {
                let handle = emitter.pdf.get_object_handle(*source);
                emitter.write_object(&handle, None)?;
            }
            PlannedIndirectObject::ObjectStream {
                origin: PlannedObjectStreamOrigin::SourceBacked(source),
                ..
            } => {
                let handle = emitter.pdf.get_object_handle(*source);
                emitter.write_object(&handle, None)?;
            }
            PlannedIndirectObject::ObjectStream {
                origin,
                output,
                members,
            } => {
                // Generated containers have no canonical source identity yet.
                // Keep their existing owner until the allocation/queue cutover;
                // never fabricate a source identity from the output number.
                emitter.emit_planned_object_stream(origin, *output, members)?;
            }
        }
    }

    Ok((bytes, layout))
}

struct PlainObjectEmitter<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    options: &'a WriterOptions,
    plan: &'a PlainWritePlan,
    bytes: &'a mut Vec<u8>,
    layout: &'a mut BodyLayout,
    lengths: BTreeMap<u32, usize>,
    object_stream_to_objects: BTreeMap<
        u32,
        (
            &'a PlannedObjectStreamOrigin,
            ObjectRef,
            &'a [PlannedMember],
        ),
    >,
    encryption: crate::writer::encryption_state::WriterEncryptionState,
}

impl<'a, R: Read + Seek + 'static> crate::writer::write_object::WriteObject
    for PlainObjectEmitter<'a, R>
{
    type ObjectStreamContainer = (
        &'a PlannedObjectStreamOrigin,
        ObjectRef,
        &'a [PlannedMember],
    );

    fn object_stream_container(&self, object: ObjectRef) -> Option<Self::ObjectStreamContainer> {
        self.object_stream_to_objects.get(&object.number).copied()
    }

    fn write_object_stream(
        &mut self,
        _object: &ObjectHandle,
        container: Self::ObjectStreamContainer,
    ) -> crate::Result<()> {
        self.emit_planned_object_stream(container.0, container.1, container.2)
    }

    fn indicate_progress(&mut self) -> crate::Result<()> {
        crate::writer::report_progress_event(self.options)
    }

    fn output_number(&self, object: ObjectRef) -> crate::Result<u32> {
        self.plan
            .new_for_original(object)
            .map(|output| output.number)
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer: reference {} {} R absent from renumber map",
                    object.number, object.generation
                ))
            })
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> crate::Result<()> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn output_count(&self) -> usize {
        self.bytes.len()
    }
    fn xref(&mut self) -> &mut BTreeMap<u32, (u16, usize)> {
        &mut self.layout.uncompressed
    }
    fn lengths(&mut self) -> &mut BTreeMap<u32, usize> {
        &mut self.lengths
    }
    fn encryption_state(&mut self) -> &mut crate::writer::encryption_state::WriterEncryptionState {
        &mut self.encryption
    }

    fn unparse_object(
        &mut self,
        object: &ObjectHandle,
        _in_object_stream: bool,
    ) -> crate::Result<()> {
        emit_source_from_handle(object, self.options, self.plan, self.bytes)
    }
}

impl<R: Read + Seek + 'static> PlainObjectEmitter<'_, R> {
    fn emit_planned_object_stream(
        &mut self,
        origin: &PlannedObjectStreamOrigin,
        output: ObjectRef,
        members: &[PlannedMember],
    ) -> crate::Result<()> {
        let pdf = &mut *self.pdf;
        let options = self.options;
        let plan = self.plan;
        let bytes = &mut *self.bytes;
        let layout = &mut *self.layout;
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
                let result = if handle.object_ref() == plan.root_source {
                    handle.write_root_object_with_ref_map_and_removed(
                        out,
                        &map,
                        &plan.removed_refs,
                        &plan.version,
                        plan.final_extension_level,
                    )
                } else {
                    handle.write_object_with_ref_map_and_removed(out, &map, &plan.removed_refs)
                };
                if result.is_ok() {
                    crate::writer::report_progress_event(options)?;
                }
                result
            },
        )?;
        let offset = bytes.len();
        bytes
            .extend_from_slice(format!("{} {} obj\n", output.number, output.generation).as_bytes());
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
            bytes,
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
        Ok(())
    }
}

fn emit_source_from_handle(
    handle: &ObjectHandle,
    options: &WriterOptions,
    plan: &PlainWritePlan,
    bytes: &mut Vec<u8>,
) -> crate::Result<()> {
    handle.try_dereference()?;
    let source = handle.object_ref();
    let map = |object_ref| {
        plan.new_for_original(object_ref).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "plain writer: reference {} {} R absent from renumber map",
                object_ref.number, object_ref.generation
            ))
        })
    };

    if plan.root_source == source {
        handle.write_root_object_with_ref_map_and_removed(
            bytes,
            &map,
            &plan.removed_refs,
            &plan.version,
            plan.final_extension_level,
        )?; // cov:ignore: root serializer success is exercised by qpdf parity; LLVM maps this multiline continuation to an unhit region
        return Ok(());
    }

    if handle.as_stream_dict().is_some() {
        let cached = if let Some(cached) =
            source.and_then(|source| plan.cached_stream_outputs.get(&source))
        {
            if cached.fingerprint == crate::writer::plain::plan::stream_cache_fingerprint(handle)? {
                Some(cached)
            } else {
                None
            }
        } else {
            None // cov:ignore: every planned indirect source stream receives a cache entry; structural streams bypass this emitter
        };
        let (dict, data, dictionary_options) = if let Some(cached) = cached {
            (
                cached.dict.clone(),
                cached.data.clone(),
                cached.dictionary_options,
            )
        } else {
            canonical_stream_output(handle, options)?
        };
        dict.write_stream_body_with_ref_map_and_removed_with_options(
            bytes,
            dictionary_options,
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
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
    let (dict, data, dictionary_options) =
        canonical_stream_output_with_status(handle, options, true, false)?;
    Ok((dict, data, dictionary_options))
}

pub(crate) fn canonical_stream_output_with_status(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
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
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
    let (dict, data, dictionary_options) =
        canonical_stream_output_for_rewrite_with_status(handle, options, normalize_content)?;
    Ok((dict, data, dictionary_options))
}

pub(crate) fn canonical_stream_output_for_rewrite_with_status(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
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
        crate::pdf_syntax::write_string_value(out, value);
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
/// stream dictionaries do not need a separate value materialization bridge.
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
// the direct stream factory in `reader.rs`), which -- like
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
    // writer path this replaced framed every direct stream node uniformly
    // regardless of nesting position, so
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
}

fn write_content_key(out: &mut Vec<u8>, key: &[u8]) {
    out.push(b'/');
    let key = key.strip_prefix(b"/").unwrap_or(key);
    crate::pdf_syntax::write_name_escaped(out, key);
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
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
    let (dict, data, dictionary_options) =
        canonical_stream_output_for_linearization_with_status(handle, options, normalize_content)?;
    Ok((dict, data, dictionary_options))
}

pub(crate) fn canonical_stream_output_for_linearization_with_status(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
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
    canonical_stream_filter_probe(
        handle,
        options,
        apply_full_rewrite_metadata_policy,
        normalize_content,
    )
}

/// Consume the writer's qpdf-shaped stream probe without retaining its bytes.
///
/// `QPDFWriter::writeLinearized` calls `QPDF::optimize` with a
/// `skip_stream_parameters` callback. That callback delegates to
/// `willFilterStream`, which pipes a token-filtered stream once before either
/// outer linearization pass (`QPDFWriter.cc:2543-2553`, `1239-1314`). qpdf's
/// `ValueSetter` is stateful, so the later pass observes the filter's consumed
/// state. The linearized route must preserve that ownership and timing; the
/// plain writer instead caches the produced bytes and therefore must continue
/// to use [`canonical_stream_will_be_refiltered_with_policy`].
pub(crate) fn canonical_stream_filter_probe_for_linearization(
    handle: &ObjectHandle,
    options: &WriterOptions,
    normalize_content: bool,
) -> crate::Result<bool> {
    canonical_stream_filter_probe(handle, options, true, normalize_content)
}

fn canonical_stream_filter_probe(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<bool> {
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
        let success = handle
            .pipe_stream_data(
                &mut discard,
                &mut filtering_attempted,
                attempt_encode_flags,
                attempt_decode_level,
                true,
                attempt == 1,
            )
            .map_err(|error| stream_data_error(handle, error))?;
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
) -> crate::Result<(ObjectHandle, Vec<u8>, StreamDictionaryOptions)> {
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
                let success = source_for_pipe
                    .pipe_stream_data(
                        &mut buffer,
                        &mut filtering_attempted,
                        attempt_encode_flags,
                        attempt_decode_level,
                        false,
                        attempt == 1,
                    )
                    .map_err(|error| stream_data_error(&source_for_pipe, error))?; // cov:ignore: filter-pipeline failures are covered at the pipeline boundary, not by this validated emitter

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
                canonical_stream_source_data(&source_for_pipe)
                    .map_err(|error| stream_data_error(&source_for_pipe, error))?,
                false,
                false,
            )
        };
    let mut entries = stream_dict.try_as_dictionary()?.unwrap_or_default();
    entries.insert(
        b"/Length".to_vec(),
        ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
    );
    let dict = ObjectHandle::dictionary(entries.into_iter().collect());
    let dictionary_options = StreamDictionaryOptions::new(
        filtering_attempted,
        filtering_attempted && matches!(policy, Some(CompressStreams::Yes)) && !normalized_content,
    );
    Ok((dict, data, dictionary_options))
}

/// Read a stream through qpdf's writer-owned unfiltered pipe.
///
/// `QPDFWriter::willFilterStream` still calls `pipeStreamData` once when
/// `filter_on_write` is false, passing `will_retry=true` for that first
/// attempt (`libqpdf/QPDFWriter.cc:1254-1314`). This is observably different
/// from `getRawStreamData`, whose public accessor passes `will_retry=false`
/// (`libqpdf/QPDF_Stream.cc:362-376`). In particular, a retry-aware provider
/// may return `false` after writing its bytes; qpdf's writer keeps that buffer
/// because filtering is already disabled and does not enter the retry branch.
/// Keep the source success bit out of this writer result for the same reason.
fn canonical_stream_source_data(handle: &ObjectHandle) -> crate::Result<Vec<u8>> {
    let mut buffer = crate::pipeline::buffer::Buffer::new("canonical writer stream", None);
    let mut filtering_attempted = false;
    let _source_success = handle.pipe_stream_data(
        &mut buffer,
        &mut filtering_attempted,
        0,
        crate::writer::DecodeLevel::None,
        false,
        true,
    )?; // cov:ignore: LLVM attributes this covered qpdf-shaped multiline call terminator without an executable counter
    Ok(buffer.take_buffer()?.to_vec())
}

fn stream_data_error(handle: &ObjectHandle, error: crate::Error) -> crate::Error {
    if let Some(object_ref) = handle.object_ref() {
        crate::Error::System(format!(
            "error while getting stream data for {object_ref}: {error}"
        ))
    } else {
        // cov:ignore: qpdf writer stream errors are always attributed to indirect object handles
        error
    }
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
    // QPDFWriter::willFilterStream first derives a filter request from the
    // stream and writer state, then lets QPDF_Stream::filter_on_write veto
    // every filtering branch (`QPDFWriter.cc:1254-1285`). Keep that veto
    // ahead of metadata, normalization, and compression policy construction:
    // false means raw source dispatch regardless of any of those settings.
    if !handle.get_filter_on_write()? {
        return Ok(None);
    }
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
        && !normalized_content;
    // qpdf's writer policy inspects only the stream's `/Filter` value; the
    // external-file `/F`, `/FFilter`, and `/FDecodeParms` entries remain
    // ordinary dictionary keys and do not veto this optimization
    // (`QPDFWriter.cc:1260-1269`).
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
mod final_handle_tests {
    use super::emit_content_container_from_handle_with_ref_map;
    use crate::writer::{NewlineBeforeEndstream, WriterOptions};
    use crate::ObjectHandle;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    #[test]
    fn content_container_emits_direct_streams_and_uses_the_handle_string_writer() {
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (b"/Length".to_vec(), ObjectHandle::integer(4)),
                (b"/Label".to_vec(), ObjectHandle::string(b"data".to_vec())),
            ]),
            Rc::new(b"data".to_vec()),
        );
        let container = ObjectHandle::dictionary(vec![(b"/Contents".to_vec(), stream)]);
        let options = WriterOptions {
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterOptions::default()
        };
        let map = |object_ref| Ok(object_ref);
        let mut output = Vec::new();

        emit_content_container_from_handle_with_ref_map(
            &container,
            &options,
            &mut output,
            &map,
            &BTreeSet::new(),
        )
        .expect("direct content stream emission");

        assert!(output
            .windows(b"/Contents".len())
            .any(|window| window == b"/Contents"));
        assert!(output
            .windows(b"stream\ndata\nendstream".len())
            .any(|window| { window == b"stream\ndata\nendstream" }));
    }
}

#[cfg(test)]
mod object_emitter_tests {
    use super::*;
    use std::io::Cursor;

    fn pdf() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(
            include_bytes!("../../../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .unwrap()
    }

    fn with_emitter(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        plan: &PlainWritePlan,
        check: impl FnOnce(&mut PlainObjectEmitter<'_, Cursor<Vec<u8>>>),
    ) {
        let options = WriterOptions::default();
        let mut bytes = Vec::new();
        let mut layout = BodyLayout::default();
        let mut emitter = PlainObjectEmitter {
            pdf,
            options: &options,
            plan,
            bytes: &mut bytes,
            layout: &mut layout,
            lengths: BTreeMap::new(),
            object_stream_to_objects: BTreeMap::new(),
            encryption: crate::writer::encryption_state::WriterEncryptionState::new(
                false,
                Vec::new(),
                false,
                0,
                0,
            ),
        };
        check(&mut emitter);
    }

    #[test]
    fn a_new_source_missing_from_the_frozen_plan_fails_before_open_object() {
        // The existing planner backend remains frozen until the live queue
        // cutover. Its lookup error must cross the shared owner unchanged.
        let mut pdf = pdf();
        let plan = PlainWritePlan::build(&mut pdf, &WriterOptions::default()).unwrap();
        let object = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(42))
            .unwrap();
        with_emitter(&mut pdf, &plan, |emitter| {
            assert!(
                matches!(emitter.write_object(&object, None), Err(crate::Error::Unsupported(message)) if message.contains("absent from renumber map"))
            );
            assert!(emitter.bytes.is_empty());
            assert!(emitter.layout.uncompressed.is_empty());
        });
    }

    #[test]
    fn existing_container_member_mapping_errors_propagate_from_the_member_unparser() {
        let mut pdf = pdf();
        let child = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(7))
            .unwrap();
        let child_id = child.object_ref().unwrap();
        let member = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/Child".to_vec(),
                child,
            )]))
            .unwrap();
        let member_id = member.object_ref().unwrap();
        pdf.root_handle()
            .unwrap()
            .replace_key(b"/Member", member)
            .unwrap();
        let mut plan = PlainWritePlan::build(&mut pdf, &WriterOptions::default()).unwrap();
        let members = [PlannedMember {
            source: member_id,
            output: plan.new_for_original(member_id).unwrap(),
        }];
        plan.old_to_new.remove(&child_id);
        with_emitter(&mut pdf, &plan, |emitter| {
            let result = emitter.emit_planned_object_stream(
                &PlannedObjectStreamOrigin::Synthetic,
                ObjectRef::new(9, 0),
                &members,
            );
            assert!(
                matches!(result, Err(crate::Error::Unsupported(message)) if message.contains("absent from renumber map"))
            );
            assert!(emitter.bytes.is_empty());
        });
    }

    #[test]
    fn source_container_extends_is_remapped_and_an_absent_target_is_reported() {
        let mut pdf = pdf();
        let target = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(7))
            .unwrap();
        let target_id = target.object_ref().unwrap();
        let member = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(42))
            .unwrap();
        let member_id = member.object_ref().unwrap();
        let source = pdf.new_stream_with_data(Rc::new(Vec::new())).unwrap();
        let source_id = source.object_ref().unwrap();
        source
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Extends", target)
            .unwrap();
        let root = pdf.root_handle().unwrap();
        root.replace_key(b"/Source", source).unwrap();
        root.replace_key(b"/Member", member).unwrap();
        let mut plan = PlainWritePlan::build(&mut pdf, &WriterOptions::default()).unwrap();
        let members = [PlannedMember {
            source: member_id,
            output: plan.new_for_original(member_id).unwrap(),
        }];
        let expected = format!(
            "/Extends {} 0 R",
            plan.new_for_original(target_id).unwrap().number
        );
        with_emitter(&mut pdf, &plan, |emitter| {
            emitter
                .emit_planned_object_stream(
                    &PlannedObjectStreamOrigin::SourceBacked(source_id),
                    ObjectRef::new(9, 0),
                    &members,
                )
                .unwrap();
            assert!(emitter
                .bytes
                .windows(expected.len())
                .any(|window| window == expected.as_bytes()));
        });
        plan.old_to_new.remove(&target_id);
        with_emitter(&mut pdf, &plan, |emitter| {
            let result = emitter.emit_planned_object_stream(
                &PlannedObjectStreamOrigin::SourceBacked(source_id),
                ObjectRef::new(9, 0),
                &members,
            );
            assert!(
                matches!(result, Err(crate::Error::Unsupported(message)) if message.contains("/Extends") && message.contains("absent from renumber map"))
            );
        });
    }

    #[test]
    fn a_canonical_null_container_source_emits_members_without_extends() {
        let mut pdf = pdf();
        let source = pdf
            .make_indirect_from_object_handle(ObjectHandle::null())
            .unwrap();
        let source_id = source.object_ref().unwrap();
        let member = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(42))
            .unwrap();
        let member_id = member.object_ref().unwrap();
        pdf.root_handle()
            .unwrap()
            .replace_key(b"/Member", member)
            .unwrap();
        let plan = PlainWritePlan::build(&mut pdf, &WriterOptions::default()).unwrap();
        let members = [PlannedMember {
            source: member_id,
            output: plan.new_for_original(member_id).unwrap(),
        }];
        with_emitter(&mut pdf, &plan, |emitter| {
            emitter
                .emit_planned_object_stream(
                    &PlannedObjectStreamOrigin::SourceBacked(source_id),
                    ObjectRef::new(9, 0),
                    &members,
                )
                .unwrap();
            assert!(emitter
                .bytes
                .windows(b"/Type /ObjStm".len())
                .any(|window| window == b"/Type /ObjStm"));
            assert!(!emitter
                .bytes
                .windows(b"/Extends".len())
                .any(|window| window == b"/Extends"));
        });
    }
}
