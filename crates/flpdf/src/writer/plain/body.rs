//! qpdf correspondence: QPDFWriter.cc plain object-body emission split from planning and xref output.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::qpdf_obj_gen::QpdfObjGen;
use crate::writer::object_streams;
use crate::writer::plain::plan::{
    stream_cache_fingerprint, PlainWritePlan, PlannedIndirectObject, PlannedMember,
    PlannedObjectStreamOrigin,
};
use crate::writer::plain::xref::{BodyLayout, CompressedLocation};
use crate::writer::write_object::{IndirectStreamLength, QdfObjectInfo, WriteObject};
use crate::writer::WriterOptions;
use crate::writer::{
    serialize, CompressStreams, ObjectWriterEmission, StreamDictionaryOptions, QPDF_BINARY_MARKER,
};
use crate::{ObjectHandle, ObjectRef, PageDocumentHelper, Pdf};

/// The qpdf standard-writer queue for the bounded plain Disable consumer, and
/// (once [`LiveQueue::register_object_streams`] has populated container
/// membership) for a Preserve source ObjStm group. Numbers are assigned when
/// a reference is first observed, and the pending queue is allowed to grow
/// while an object is being unparsed. This is the Rust counterpart of
/// `QPDFWriter::enqueueObject` plus `object_queue`.
struct LiveQueue {
    old_to_new: BTreeMap<ObjectRef, ObjectRef>,
    raw_old_to_new: BTreeMap<QpdfObjGen, ObjectRef>,
    pending: VecDeque<ObjectHandle>,
    removed_refs: BTreeSet<ObjectRef>,
    /// Source ObjGen of an ObjStm member -> source ObjGen of its container.
    /// Empty for the plain Disable consumer, which has no compressed members.
    member_to_container: BTreeMap<ObjectRef, ObjectRef>,
    /// Source ObjGen of an ObjStm container -> its members' source ObjGens,
    /// in qpdf's `std::set<QPDFObjGen>` (ascending) order. Empty for Disable.
    container_to_members: BTreeMap<ObjectRef, Vec<ObjectRef>>,
    /// Members whose container chain is still being resolved.
    ///
    /// qpdf marks the same state by storing the invalid object ID `0` in
    /// `obj_renumber` before it recurses, and ignores the object when it meets
    /// that sentinel again (`libqpdf/QPDFWriter.cc:1097-1124`). flpdf derives
    /// the next output number from `old_to_new.len()`, so a sentinel entry
    /// there would shift every later number; keep the mark in its own set.
    resolving_members: BTreeSet<ObjectRef>,
}

impl LiveQueue {
    /// Allocate the next output object number.
    ///
    /// Both maps name objects in the same output number space, so the counter
    /// has to span them: numbering the ordinary map alone hands a number that a
    /// raw-identity object already holds, and the later xref entry overwrites
    /// the earlier one, dropping an object from the file.
    fn next_output_number(&self) -> ObjectRef {
        ObjectRef::new(
            (self.old_to_new.len() + self.raw_old_to_new.len() + 1) as u32,
            0,
        )
    }

    fn new(removed_refs: BTreeSet<ObjectRef>) -> Self {
        Self {
            old_to_new: BTreeMap::new(),
            raw_old_to_new: BTreeMap::new(),
            pending: VecDeque::new(),
            removed_refs,
            member_to_container: BTreeMap::new(),
            container_to_members: BTreeMap::new(),
            resolving_members: BTreeSet::new(),
        }
    }

    /// Register qpdf's source-backed or generated ObjStm membership so that
    /// [`Self::enqueue_handle`] redirects a member's discovery to its
    /// container instead of numbering the member as a plain indirect object,
    /// matching `QPDFWriter::enqueueObject`'s member branch
    /// (`libqpdf/QPDFWriter.cc:1071-1132`).
    fn register_object_streams(
        &mut self,
        groups: &[crate::writer::object_streams::ObjectStreamGroup],
    ) {
        for group in groups {
            let (source, members) = match group {
                crate::writer::object_streams::ObjectStreamGroup::SourceBacked {
                    source,
                    members,
                }
                | crate::writer::object_streams::ObjectStreamGroup::Generated { source, members } => {
                    (*source, members)
                }
                crate::writer::object_streams::ObjectStreamGroup::Synthetic { .. } => {
                    // cov:ignore-start: the specialized live route materializes generated groups with a qpdf-shaped null source before registration; the plain Preserve route never supplies a synthetic group.
                    continue;
                    // cov:ignore-end
                }
            };
            // qpdf excludes a removed/ineligible member from
            // `object_to_object_stream` before the enqueue walk begins
            // (`preserveObjectStreams`, `libqpdf/QPDFWriter.cc:1957-1966`),
            // so it is never treated as a compressed member at all. Mirror
            // that exclusion here rather than letting a later removed-ref
            // check race the eager numbering loop in `enqueue_handle`.
            let retained: Vec<ObjectRef> = members
                .iter()
                .copied()
                .filter(|member| !self.removed_refs.contains(member))
                .collect();
            if retained.is_empty() {
                continue;
            }
            for member in &retained {
                self.member_to_container.insert(*member, source);
            }
            self.container_to_members.insert(source, retained);
        }
    }

    fn enqueue_handle<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        handle: ObjectHandle,
    ) -> crate::Result<Option<ObjectRef>> {
        if handle.owning_pdf_unique_id() != Some(pdf.unique_id()) {
            return Err(crate::Error::Internal(
                "QPDFObjectHandle from different QPDF found while writing.  Use QPDF::copyForeignObject to add objects from another file."
                    .to_string(),
            ));
        }
        // cov:ignore-start: only indirect handles are enqueued by qpdf's object queue.
        let Some(source) = handle.object_ref() else {
            let Some(raw_source) = handle
                .qpdf_obj_gen()
                .filter(|object_gen| object_gen.is_indirect())
            else {
                return Ok(None);
            };
            if let Some(output) = self.raw_old_to_new.get(&raw_source).copied() {
                return Ok(Some(output));
            }
            let output = self.next_output_number();
            self.raw_old_to_new.insert(raw_source, output);
            self.pending.push_back(handle);
            return Ok(Some(output));
        };
        // cov:ignore-end
        // cov:ignore-start: qpdf never registers object number zero as a live object.
        if source.number == 0 {
            return Ok(None);
        }
        // cov:ignore-end
        // qpdf has no removed-reference guard in enqueueObject: a source
        // ObjStm container is still enqueued after a member redirects to it,
        // so assignCompressedObjectNumbers can reserve every retained member
        // (`QPDFWriter.cc:1057-1124`). The planner removes members before
        // registration; preserve the local removal filter only for ordinary
        // objects and unregistered removed members.
        if self.removed_refs.contains(&source) && !self.container_to_members.contains_key(&source) {
            return Ok(None);
        }
        if let Some(output) = self.old_to_new.get(&source).copied() {
            return Ok(Some(output));
        }
        if let Some(&container) = self.member_to_container.get(&source) {
            // A member is never queued or numbered on its own; discovering it
            // enqueues its container instead, which eagerly numbers every
            // member of that container below
            // (`QPDFWriter::assignCompressedObjectNumbers`,
            // `libqpdf/QPDFWriter.cc:1057-1069`).
            //
            // A specially constructed file can name a container that is itself
            // a member, directly or through a chain, so this recursion is not
            // bounded by construction. qpdf guards it by storing the invalid
            // object ID `0` before recursing and ignoring the object when it
            // meets that sentinel again (`:1097-1104,1120-1124`).
            if !self.resolving_members.insert(source) {
                // qpdf leaves the invalid object-number sentinel in
                // `obj_renumber` and later serializes references to it as
                // `0 0 R`; returning a missing queue result instead turns
                // the same damaged graph into a writer exception.
                return Ok(Some(ObjectRef::new(0, 0)));
            }
            let container_handle = pdf.get_object_handle(container);
            self.enqueue_handle(pdf, container_handle)?;
            return Ok(self.old_to_new.get(&source).copied());
        }
        let output = self.next_output_number();
        self.old_to_new.insert(source, output);
        self.pending.push_back(handle);
        if let Some(members) = self.container_to_members.get(&source).cloned() {
            for member in members {
                if !self.old_to_new.contains_key(&member) {
                    let member_output = self.next_output_number();
                    self.old_to_new.insert(member, member_output);
                }
            }
        }
        Ok(Some(output))
    }

    fn pop(&mut self) -> Option<ObjectHandle> {
        self.pending.pop_front()
    }
}

/// Output of the live Disable body pass. The trailer/xref layer consumes the
/// completed map only after the queue has stopped growing.
pub(crate) struct LiveBodyOutput {
    pub(crate) bytes: Vec<u8>,
    pub(crate) layout: BodyLayout,
    pub(crate) old_to_new: BTreeMap<ObjectRef, ObjectRef>,
    pub(crate) object_count: usize,
}

fn collect_live_seed_handles(
    handle: &ObjectHandle,
    found: &mut Vec<ObjectHandle>,
    depth: usize,
) -> crate::Result<()> {
    if handle.object_ref().is_some() {
        found.push(handle.clone());
        return Ok(());
    }
    // The parser bounds nesting in parsed input, but a library-built inline
    // root or trailer value could nest arbitrarily (or form a direct cycle).
    // Bound the direct-seed recursion the same way the parser does rather than
    // overflow the stack.
    if depth > crate::parser::MAX_PARSE_DEPTH {
        // cov:ignore-start: defensive stack bound; parsed input is parser-capped and factory-built seed trees are acyclic, so this overflow arm is unreachable from the corpus.
        return Err(crate::Error::Unsupported(format!(
            "plain live writer: direct seed nesting exceeds maximum of {}",
            crate::parser::MAX_PARSE_DEPTH
        )));
        // cov:ignore-end
    }
    if let Some(items) = handle.try_as_array()? {
        for item in items {
            collect_live_seed_handles(&item, found, depth + 1)?;
        }
    } else if let Some(stream_dict) = handle.as_stream_dict() {
        // A direct stream reaches the live queue through the indirect children
        // of its dictionary, matching qpdf's enqueueObject direct recursion
        // (`QPDFWriter.cc:1129-1147`). `try_as_dictionary` does not view a
        // stream as a dictionary, so descend the stream dictionary explicitly.
        // cov:ignore-start: defensive descent into a direct stream's dictionary -- parsed streams are indirect (taken by the base case above) and an in-memory stream surfaces its dictionary through the `try_as_dictionary` arm below, so this body is unreachable from the corpus.
        for (_, value) in stream_dict.try_as_dictionary()?.unwrap_or_default() {
            if !value.try_is_null()? {
                collect_live_seed_handles(&value, found, depth + 1)?;
            }
        }
        // cov:ignore-end
    } else if let Some(entries) = handle.try_as_dictionary()? {
        for (_, value) in entries {
            if !value.try_is_null()? {
                collect_live_seed_handles(&value, found, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn seed_live_queue<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    queue: &mut LiveQueue,
    options: &WriterOptions,
) -> crate::Result<()> {
    if options.preserve_unreferenced_objects {
        for handle in pdf.get_all_objects()? {
            queue.enqueue_handle(pdf, handle)?;
        }
    }

    let root = pdf.root_handle()?;
    let mut root_seeds = Vec::new();
    collect_live_seed_handles(&root, &mut root_seeds, 0)?;
    for handle in root_seeds {
        queue.enqueue_handle(pdf, handle)?;
    }

    let trailer = pdf.trailer();
    let trailer_entries = trailer.try_as_dictionary()?.unwrap_or_default();
    for (key, value) in trailer_entries {
        if matches!(
            key.as_slice(),
            b"/ID"
                | b"/Size"
                | b"/Encrypt"
                | b"/Prev"
                | b"/Root"
                | b"/Type"
                | b"/F"
                | b"/FFilter"
                | b"/FDecodeParms"
                | b"/W"
                | b"/Index"
                | b"/Length"
                | b"/Filter"
                | b"/DecodeParms"
                | b"/XRefStm"
        ) || value.try_is_null()?
        {
            continue;
        }
        let mut handles = Vec::new();
        collect_live_seed_handles(&value, &mut handles, 0)?;
        for handle in handles {
            queue.enqueue_handle(pdf, handle)?;
        }
    }
    Ok(())
}

/// Emit the plain Disable body using qpdf's live queue. Direct values are
/// traversed only when they are queue seeds; indirect children are discovered
/// by the writer-owned unparser while each queued object is emitted.
pub(crate) fn emit_live_disable<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    version: &str,
    final_extension_level: i64,
    root_source: Option<ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
    object_streams: &[crate::writer::object_streams::ObjectStreamGroup],
) -> crate::Result<LiveBodyOutput> {
    emit_live_body(
        pdf,
        options,
        version,
        final_extension_level,
        root_source,
        removed_refs,
        object_streams,
        None,
        false,
        false,
    )
}

/// Emit a PCLm body using the same live queue and `WriteObject` owner as the
/// ordinary standard writer. Only the initial seed order is PCLm-specific;
/// descendants are discovered while each queued object is written.
pub(crate) fn emit_live_pclm<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    version: &str,
    final_extension_level: i64,
    root_source: Option<ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
) -> crate::Result<LiveBodyOutput> {
    emit_live_body(
        pdf,
        options,
        version,
        final_extension_level,
        root_source,
        removed_refs,
        &[],
        None,
        true,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_live_body<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    version: &str,
    final_extension_level: i64,
    root_source: Option<ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
    object_streams: &[crate::writer::object_streams::ObjectStreamGroup],
    encryption_context: Option<&crate::writer::EncryptionContext>,
    pclm: bool,
    two_pass_object_streams: bool,
) -> crate::Result<LiveBodyOutput> {
    let mut queue = LiveQueue::new(removed_refs.clone());
    queue.register_object_streams(object_streams);
    if pclm {
        for handle in crate::writer::pclm::seed_handles(pdf)? {
            queue.enqueue_handle(pdf, handle)?;
        }
    } else {
        seed_live_queue(pdf, &mut queue, options)?;
    }

    let mut bytes = Vec::new();
    if pclm {
        bytes.extend_from_slice(format!("%PDF-{version}\n%PCLm 1.0\n").as_bytes());
    } else {
        bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
        bytes.extend_from_slice(QPDF_BINARY_MARKER);
    }
    bytes.extend_from_slice(options.extra_header_text.as_bytes());
    if pclm && !options.extra_header_text.is_empty() && !options.extra_header_text.ends_with('\n') {
        bytes.push(b'\n');
    }
    let mut layout = BodyLayout::default();
    let (encryption, encrypted_strings) = if let Some(ctx) = encryption_context {
        (
            crate::writer::encryption_state::WriterEncryptionState::new(
                true,
                ctx.file_key.clone(),
                crate::writer::cipher_needs_aes_iv(ctx.cipher),
                ctx.encryption_v,
                ctx.encryption_r,
            ),
            Some(crate::writer::encrypted_strings::EncryptedStringEmitter::from_context(ctx)),
        )
    } else {
        (
            crate::writer::encryption_state::WriterEncryptionState::new(
                false,
                Vec::new(),
                false,
                0,
                0,
            ),
            None,
        )
    };
    let mut emitter = LiveObjectEmitter {
        pdf,
        options,
        bytes: &mut bytes,
        layout: &mut layout,
        queue: RefCell::new(queue),
        root_source,
        version,
        final_extension_level,
        removed_refs,
        lengths: BTreeMap::new(),
        encryption,
        encryption_context,
        encrypted_strings,
        pclm,
        two_pass_object_streams,
        current_raw_output: None,
    };
    loop {
        let source = emitter.queue.borrow_mut().pop();
        let Some(handle) = source else { break };
        emitter.current_raw_output = handle.qpdf_obj_gen().and_then(|object_gen| {
            emitter
                .queue
                .borrow()
                .raw_old_to_new
                .get(&object_gen)
                .copied()
        });
        emitter.write_object(&handle, None)?;
        emitter.current_raw_output = None;
    }
    let queue = emitter.queue.into_inner();
    let old_to_new = queue.old_to_new;
    let object_count = old_to_new.len() + queue.raw_old_to_new.len();
    Ok(LiveBodyOutput {
        bytes,
        layout,
        old_to_new,
        object_count,
    })
}

/// Emit a specialized non-linearized standard body through the same live queue
/// as plain Disable/Preserve. The queue is deliberately shared: qpdf's
/// `enqueueObject` and `unparseChild` do not change their reachability contract
/// when encryption or an extra header is selected; those are writer framing
/// dimensions layered around the same queue (`QPDFWriter.cc:1057-1157,
/// 1761-1809, 2907-2925`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_live_specialized_standard<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    version: &str,
    final_extension_level: i64,
    root_source: Option<ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
    object_streams: &[crate::writer::object_streams::ObjectStreamGroup],
    encryption_context: Option<&crate::writer::EncryptionContext>,
) -> crate::Result<LiveBodyOutput> {
    emit_live_body(
        pdf,
        options,
        version,
        final_extension_level,
        root_source,
        removed_refs,
        object_streams,
        encryption_context,
        false,
        true,
    )
}

fn qdf_page_context<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> crate::Result<(BTreeMap<ObjectRef, usize>, BTreeMap<ObjectRef, usize>)> {
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    let mut page_sequences = BTreeMap::new();
    let mut contents_sequences = BTreeMap::new();
    for (index, page) in pages.into_iter().enumerate() {
        let sequence = index
            .checked_add(1)
            .ok_or_else(|| crate::Error::Unsupported("QDF page sequence overflows usize".into()))?;
        page_sequences.insert(page, sequence);
        for content in crate::writer::collect_content_stream_refs(pdf, page)? {
            contents_sequences.insert(content, sequence);
        }
    }
    Ok((page_sequences, contents_sequences))
}

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
    // `contents_sequences` identifies page content streams for normalization,
    // which content-normalization needs whether or not QDF framing is on; only
    // the `%% Page N` comments themselves are QDF-only. Building this map for
    // QDF alone left the normalization fallback with an empty map, so a cache
    // miss would emit the stream unnormalized.
    let (page_sequences, contents_sequences) = if options.qdf || options.content_normalization {
        qdf_page_context(pdf)?
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{}\n", plan.version).as_bytes());
    bytes.extend_from_slice(QPDF_BINARY_MARKER);
    if options.qdf {
        bytes.extend_from_slice(b"%QDF-1.0\n\n");
    }

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
                    origin:
                        origin @ (PlannedObjectStreamOrigin::SourceBacked(source)
                        | PlannedObjectStreamOrigin::Generated(source)),
                    output,
                    members,
                } => Some((source.number, (origin, *output, members.as_slice()))),
                _ => None,
            })
            .collect(),
        page_sequences,
        contents_sequences,
        current_stream_length: None,
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
            PlannedIndirectObject::RawSource { raw, output, .. } => {
                emitter.emit_raw_source(*raw, *output)?;
            }
            PlannedIndirectObject::ObjectStream {
                origin:
                    PlannedObjectStreamOrigin::SourceBacked(source)
                    | PlannedObjectStreamOrigin::Generated(source),
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
                // Synthetic containers have no canonical source identity.
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
    page_sequences: BTreeMap<ObjectRef, usize>,
    contents_sequences: BTreeMap<ObjectRef, usize>,
    current_stream_length: Option<IndirectStreamLength>,
    encryption: crate::writer::encryption_state::WriterEncryptionState,
}

struct LiveObjectEmitter<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    options: &'a WriterOptions,
    bytes: &'a mut Vec<u8>,
    layout: &'a mut BodyLayout,
    queue: RefCell<LiveQueue>,
    root_source: Option<ObjectRef>,
    version: &'a str,
    final_extension_level: i64,
    removed_refs: BTreeSet<ObjectRef>,
    lengths: BTreeMap<u32, usize>,
    encryption: crate::writer::encryption_state::WriterEncryptionState,
    encryption_context: Option<&'a crate::writer::EncryptionContext>,
    encrypted_strings: Option<crate::writer::encrypted_strings::EncryptedStringEmitter>,
    pclm: bool,
    two_pass_object_streams: bool,
    current_raw_output: Option<ObjectRef>,
}

/// Direct-stream policy for the specialized live object walk. Ordinary
/// `ObjectHandle` emission only needs a value serializer, but qpdf's
/// `unparseChild` recurses into a direct Stream and writes its payload inline.
/// Keep filtering, `/Length`, newline, and encryption decisions at this writer
/// boundary where the current object output number and `WriterOptions` exist.
struct LiveDirectStreamWriter<'a> {
    options: &'a WriterOptions,
    output: ObjectRef,
    encryption_context: Option<&'a crate::writer::EncryptionContext>,
}

fn pclm_stream_parts(stream: &ObjectHandle) -> crate::Result<(ObjectHandle, Vec<u8>)> {
    stream.try_dereference()?;
    let dict = stream
        .as_stream_dict()
        .ok_or_else(|| crate::Error::Internal("PCLm stream dictionary is missing".to_string()))?
        .unsafe_shallow_copy()?;
    let data = stream.get_raw_stream_data()?.as_ref().to_vec();
    dict.replace_key(
        b"/Length",
        ObjectHandle::integer(i64::try_from(data.len()).map_err(|_| {
            // cov:ignore-start: Vec payloads on supported targets fit in i64.
            crate::Error::Unsupported("PCLm stream /Length does not fit in i64".to_string())
            // cov:ignore-end
        })?), // cov:ignore: Vec payloads on supported targets fit in i64.
    )?; // cov:ignore: the validated PCLm stream dictionary replacement cannot fail.
    Ok((dict, data))
}

impl crate::writer::object::DynamicDirectStreamWriter for LiveDirectStreamWriter<'_> {
    fn write_direct_stream(
        &mut self,
        stream: &ObjectHandle,
        out: &mut Vec<u8>,
        map: &mut crate::writer::object::DynamicObjectRefMap<'_>,
        removed_refs: &BTreeSet<ObjectRef>,
        write_string: &mut dyn FnMut(&mut Vec<u8>, &[u8]) -> crate::Result<()>,
    ) -> crate::Result<()> {
        stream.try_dereference()?;
        let (dict, data, dictionary_options) = if self.options.pclm {
            let (dict, data) = pclm_stream_parts(stream)?;
            (dict, data, StreamDictionaryOptions::preserve())
        } else {
            canonical_stream_output(stream, self.options)?
        };
        let stream_encryption = self.encryption_context;
        let is_metadata_stream = dict.try_is_dictionary_of_type(b"Metadata", b"")?;
        // qpdf exempts only a stream whose dictionary is /Type /Metadata when
        // EncryptMetadata is false. A direct Stream nested in an ordinary
        // object remains encrypted (`QPDFWriter.cc:1251-1278,1545-1556`).
        let encrypt_stream =
            stream_encryption.is_some_and(|ctx| ctx.encrypt_metadata || !is_metadata_stream);
        let mut stream_length = data.len();
        if let Some(ctx) = stream_encryption {
            crate::writer::adjust_aes_stream_length(&mut stream_length, ctx, encrypt_stream)?;
        }
        dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                // cov:ignore-start: an allocatable direct stream payload fits in i64
                crate::Error::Unsupported("direct stream /Length does not fit in i64".to_string())
                // cov:ignore-end
            })?), // cov:ignore: allocatable direct stream payloads fit in i64; LLVM attributes the covered conversion continuation here.
        )?; // cov:ignore: the validated direct-stream dictionary replacement is covered; LLVM attributes its multiline terminator here.
        let mut write_string_wrapper = |out: &mut Vec<u8>, value: &[u8]| write_string(out, value);
        dict.write_stream_body_with_dynamic_ref_map_and_string_writer(
            out,
            dictionary_options,
            map,
            removed_refs,
            &mut write_string_wrapper,
        )?; // cov:ignore: the validated direct-stream dictionary serializer is covered; LLVM attributes its multiline terminator here.
        if let Some(ctx) = stream_encryption {
            crate::writer::write_stream_payload_with_pipeline(
                out,
                &data,
                self.options.newline_before_endstream,
                self.output,
                ctx,
                encrypt_stream,
                None,
            )?; // cov:ignore: the validated encrypted direct-stream pipeline is covered; LLVM attributes its multiline terminator here.
        } else {
            serialize::write_stream_payload(out, &data, self.options.newline_before_endstream);
        }
        Ok(())
    }
}

impl<'a, R: Read + Seek + 'static> crate::writer::write_object::WriteObject
    for LiveObjectEmitter<'a, R>
{
    type ObjectStreamContainer = Vec<ObjectRef>;

    fn object_stream_container(&self, object: ObjectRef) -> Option<Vec<ObjectRef>> {
        self.queue
            .borrow()
            .container_to_members
            .get(&object)
            .cloned()
    }

    fn write_object_stream(
        &mut self,
        object: &ObjectHandle,
        container: Vec<ObjectRef>,
    ) -> crate::Result<()> {
        self.emit_live_object_stream(object, &container)
    }

    fn indicate_progress(&mut self) -> crate::Result<()> {
        crate::writer::report_progress_event(self.options)
    }

    fn output_number(&self, object: ObjectRef) -> crate::Result<u32> {
        // cov:ignore-start: llvm-coverage attributes this raw-orphan branch's
        // closing guard to the integration-tested return path.
        if object == ObjectRef::new(0, 0) {
            if let Some(output) = self.current_raw_output {
                return Ok(output.number);
            }
        }
        // cov:ignore-end
        self.queue
            .borrow()
            .old_to_new
            .get(&object)
            .map(|output| output.number)
            // cov:ignore-start: queue insertion precedes emission, so every emitted source is mapped.
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain live writer: reference {} {} R absent from queue",
                    object.number, object.generation
                ))
            })
        // cov:ignore-end
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
        let mut map = |child: &ObjectHandle| {
            self.queue
                .borrow_mut()
                .enqueue_handle(self.pdf, child.clone())?
                // cov:ignore-start: dynamic child callbacks run only after the removed/direct filters.
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "plain live writer: child is direct or removed".to_string(),
                    )
                })
            // cov:ignore-end
        };
        let output = if let Some(source) = object.object_ref() {
            self.queue
                .borrow()
                .old_to_new
                .get(&source)
                .copied()
                // cov:ignore-start: every queued indirect object is numbered before writeObject invokes its unparser.
                .ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "plain live writer: reference {} {} R absent from queue",
                        source.number, source.generation
                    ))
                })?
            // cov:ignore-end
        } else if let Some(raw) = object.qpdf_obj_gen() {
            self.queue
                .borrow()
                .raw_old_to_new
                .get(&raw)
                .copied()
                .or(self.current_raw_output)
                // cov:ignore-start: a raw indirect handle is recorded by enqueue_handle before it reaches the unparser.
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "plain live writer: raw reference absent from queue".to_string(),
                    )
                })?
            // cov:ignore-end
        } else {
            self.current_raw_output.unwrap_or(ObjectRef::new(0, 0)) // cov:ignore: WriteObject invokes this unparser only for queued indirect or raw-indirect handles; direct values recurse inside the object serializer.
        };
        if self.pclm && object.as_stream_dict().is_some() {
            let (dict, data) = pclm_stream_parts(object)?;
            dict.write_stream_body_with_dynamic_ref_map(
                self.bytes,
                StreamDictionaryOptions::preserve(),
                &mut map,
                &self.removed_refs,
            )?; // cov:ignore: the validated PCLm stream dictionary has no reachable serialization error.
            serialize::write_stream_payload(
                self.bytes,
                &data,
                self.options.newline_before_endstream,
            );
            return Ok(());
        }
        if self.root_source == object.object_ref() {
            let mut direct_stream_writer = LiveDirectStreamWriter {
                options: self.options,
                output,
                encryption_context: self.encryption_context,
            };
            if let Some(emitter) = self.encrypted_strings.as_mut() {
                let root = object.output_root_copy_with_adbe(
                    self.version,
                    self.final_extension_level,
                    true,
                )?; // cov:ignore: LLVM attributes the covered encrypted root-copy call terminator to callback cleanup.
                emitter.write_handle_object_with_dynamic_ref_map_and_direct_stream_writer(
                    self.bytes,
                    output,
                    None,
                    &root,
                    &mut map,
                    &self.removed_refs,
                    &mut direct_stream_writer,
                )?; // cov:ignore: LLVM attributes the covered encrypted dynamic-object call terminator to callback cleanup.
            } else {
                let mut write_string = |out: &mut Vec<u8>, value: &[u8]| {
                    crate::pdf_syntax::write_string_value(out, value);
                    Ok(())
                };
                crate::writer::object::write_root_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                    object,
                    self.bytes,
                    &mut map,
                    &self.removed_refs,
                    self.version,
                    self.final_extension_level,
                    true,
                    &mut write_string,
                    &mut direct_stream_writer,
                )?; // cov:ignore: LLVM attributes root emission's call terminator to callback cleanup.
            }
        } else if object.as_stream_dict().is_some() {
            let (dict, data, dictionary_options) = canonical_stream_output(object, self.options)?;
            let source = object.object_ref();
            let stream_encryption = self.encryption_context;
            let encrypt_stream = stream_encryption
                .is_some_and(|ctx| ctx.encrypt_metadata || ctx.metadata_ref != source);
            let mut stream_length = data.len();
            if let Some(ctx) = stream_encryption {
                crate::writer::adjust_aes_stream_length(&mut stream_length, ctx, encrypt_stream)?;
            }
            dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                    // cov:ignore-start: an allocatable stream payload fits in i64
                    crate::Error::Unsupported("stream /Length does not fit in i64".to_string())
                    // cov:ignore-end
                })?), // cov:ignore: the allocated stream length fits in i64 on supported targets; this overflow arm is defensive.
            )?; // cov:ignore: validated stream /Length replacement
            if let Some(emitter) = self.encrypted_strings.as_mut() {
                emitter.write_handle_stream_dict_with_dynamic_ref_map(
                    self.bytes,
                    output,
                    None,
                    &dict,
                    dictionary_options,
                    encrypt_stream,
                    &mut map,
                    &self.removed_refs,
                )?; // cov:ignore: LLVM attributes the covered encrypted dynamic stream-dictionary call terminator to callback cleanup.
            } else {
                dict.write_stream_body_with_dynamic_ref_map(
                    self.bytes,
                    dictionary_options,
                    &mut map,
                    &self.removed_refs,
                )?; // cov:ignore: LLVM attributes stream dictionary emission's call terminator to callback cleanup.
            }
            if let Some(ctx) = stream_encryption {
                crate::writer::write_stream_payload_with_pipeline(
                    self.bytes,
                    &data,
                    self.options.newline_before_endstream,
                    output,
                    ctx,
                    encrypt_stream,
                    None,
                )?; // cov:ignore: LLVM attributes the covered encrypted stream-pipeline call terminator to callback cleanup.
            } else {
                serialize::write_stream_payload(
                    self.bytes,
                    &data,
                    self.options.newline_before_endstream,
                );
            }
        } else {
            if let Some(emitter) = self.encrypted_strings.as_mut() {
                let mut direct_stream_writer = LiveDirectStreamWriter {
                    options: self.options,
                    output,
                    encryption_context: self.encryption_context,
                };
                emitter.write_handle_object_with_dynamic_ref_map_and_direct_stream_writer(
                    self.bytes,
                    output,
                    None,
                    object,
                    &mut map,
                    &self.removed_refs,
                    &mut direct_stream_writer,
                )?; // cov:ignore: LLVM attributes the covered encrypted dynamic-object call terminator to callback cleanup.
            } else {
                let mut write_string = |out: &mut Vec<u8>, value: &[u8]| {
                    crate::pdf_syntax::write_string_value(out, value);
                    Ok(())
                };
                let mut direct_stream_writer = LiveDirectStreamWriter {
                    options: self.options,
                    output,
                    encryption_context: None,
                };
                crate::writer::object::write_object_with_dynamic_ref_map_and_string_writer_and_direct_stream_writer(
                    object,
                    self.bytes,
                    &mut map,
                    &self.removed_refs,
                    &mut write_string,
                    &mut direct_stream_writer,
                )?; // cov:ignore: LLVM attributes the covered dynamic-object call terminator to callback cleanup.
            }
        }
        Ok(())
    }
}

impl<'a, R: Read + Seek + 'static> LiveObjectEmitter<'a, R> {
    #[allow(clippy::too_many_arguments)] // qpdf separates progress phase, member identity, and live serialization policy
    fn emit_live_object_stream_member(
        &mut self,
        out: &mut Vec<u8>,
        _member_index: u32,
        _member_ref: ObjectRef,
        source: &ObjectHandle,
        report_before: bool,
        report_after: bool,
        decrement: bool,
    ) -> crate::Result<()> {
        if decrement {
            crate::writer::decrement_progress_event(self.options)?;
        }
        if report_before {
            crate::writer::report_progress_event(self.options)?;
        }
        let mut map = |child: &ObjectHandle| {
            self.queue
                .borrow_mut()
                .enqueue_handle(self.pdf, child.clone())?
                // cov:ignore-start: dynamic child callbacks run only after the removed/direct filters.
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "plain live writer: child is direct or removed".to_string(),
                    )
                })
            // cov:ignore-end
        };
        source.try_dereference()?;
        let handle = if source.as_stream_dict().is_some() {
            // qpdf's `writeObjectStream` warns once per pass and substitutes an
            // indirect null before serialization (`QPDFWriter.cc:1690-1705`).
            let warning_passes = if self.two_pass_object_streams { 1 } else { 2 };
            for _ in 0..warning_passes {
                source.warn_if_possible("stream found inside object stream; treating as null")?;
            }
            ObjectHandle::null()
        } else {
            source.clone()
        };
        let result = if handle.object_ref() == self.root_source {
            handle.write_root_object_with_dynamic_ref_map(
                out,
                &mut map,
                &self.removed_refs,
                self.version,
                self.final_extension_level,
                true,
            )
        } else {
            handle.write_object_with_dynamic_ref_map(out, &mut map, &self.removed_refs)
        };
        if report_after && result.is_ok() {
            crate::writer::report_progress_event(self.options)?;
        }
        result
    }

    /// The live-queue counterpart of `PlainObjectEmitter::emit_planned_object_stream`
    /// for a Preserve source-backed ObjStm container. The live route never
    /// runs with `options.qdf` (the dispatch gate in `mod.rs` routes QDF
    /// output through the planned writer instead), so this omits the QDF
    /// marker/pair-table framing that method carries: only the plain
    /// (`libqpdf/QPDFWriter.cc:1665-1710` non-QDF arm) shape is needed here.
    /// References are resolved through the same dynamic, discovery-time map
    /// `unparse_object` uses, since a member's own children may not yet be
    /// queued when its body is serialized
    /// (`QPDFWriter::writeObjectStream`/`unparseChild`, `libqpdf/QPDFWriter.cc:1690-1697`).
    fn emit_live_object_stream(
        &mut self,
        container: &ObjectHandle,
        members: &[ObjectRef],
    ) -> crate::Result<()> {
        let container_source = container.object_ref().ok_or_else(|| {
            // cov:ignore-start: qpdf's writeObjectStream also asserts old_og.getGen() == 0 on an indirect handle; the dispatcher only reaches here via a queued (therefore indirect) handle.
            crate::Error::Internal(
                "plain live writer: object-stream container has no source identity".to_string(),
            )
            // cov:ignore-end
        })?; // cov:ignore: the dispatcher only reaches this arm for a queued, therefore indirect, handle.
        let output = self
            .queue
            .borrow()
            .old_to_new
            .get(&container_source)
            .copied()
            // cov:ignore-start: the container's own number is assigned before it is queued, so this is always present.
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain live writer: reference {} {} R absent from queue",
                    container_source.number, container_source.generation
                ))
            })?;
        // cov:ignore-end
        let mut handles = Vec::with_capacity(members.len());
        for &member_source in members {
            let member_output = self
                .queue
                .borrow()
                .old_to_new
                .get(&member_source)
                .copied()
                // cov:ignore-start: assignCompressedObjectNumbers numbers every member when the container is first queued, above.
                .ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "plain live writer: object-stream member {} {} R absent from queue",
                        member_source.number, member_source.generation
                    ))
                })?;
            // cov:ignore-end
            let handle = self.pdf.get_object_handle(member_source);
            handles.push((member_output, handle));
        }
        if self.two_pass_object_streams {
            // qpdf's first pass decrements the anticipated count and then
            // writeObject increments it again before the member unparse. The
            // same indicateProgress(false, false) boundary is used by the
            // final pass (`QPDFWriter.cc:1639-1707,1771-1796`).
            let mut first_pass = |out: &mut Vec<u8>,
                                  member_index: u32,
                                  member_ref: ObjectRef,
                                  handle: &ObjectHandle| {
                self.emit_live_object_stream_member(
                    out,
                    member_index,
                    member_ref,
                    handle,
                    true,
                    false,
                    true,
                )
            };
            let _ = object_streams::emit_objstm_body_from_handles_with_writer(
                &handles,
                &mut first_pass,
            )?; // cov:ignore: the first-pass writer is exercised by the malformed-member test; LLVM attributes this multiline continuation here.
        }
        let mut final_pass =
            |out: &mut Vec<u8>, member_index: u32, member_ref: ObjectRef, handle: &ObjectHandle| {
                self.emit_live_object_stream_member(
                    out,
                    member_index,
                    member_ref,
                    handle,
                    self.two_pass_object_streams,
                    !self.two_pass_object_streams,
                    false,
                )
            };
        let body =
            object_streams::emit_objstm_body_from_handles_with_writer(&handles, &mut final_pass)?;
        let extends = {
            let source_handle = self.pdf.get_object_handle(container_source);
            source_handle.try_dereference()?;
            match source_handle.as_stream_dict() {
                Some(source_dict) => {
                    let extends_handle = source_dict.try_get_key(b"/Extends")?;
                    match extends_handle.object_ref() {
                        // qpdf enqueues /Extends's target the same way as any
                        // other child reference (`unparseChild`,
                        // `libqpdf/QPDFWriter.cc:1735`), so an /Extends chain
                        // to an otherwise-unreferenced predecessor container
                        // is still discovered and written.
                        Some(_) => Some(
                            self.queue
                                .borrow_mut()
                                .enqueue_handle(self.pdf, extends_handle)?
                                // cov:ignore-start: a resolved indirect /Extends always yields a live output number.
                                .ok_or_else(|| {
                                    crate::Error::Unsupported(
                                        "plain live writer: object-stream /Extends is direct or removed"
                                            .to_string(),
                                    )
                                })?,
                            // cov:ignore-end
                        ),
                        None => None,
                    }
                }
                // qpdf permits a null or otherwise non-stream source identity
                // here as a placeholder for a reconstructed object stream;
                // the live queue's Preserve source is always the real ObjStm
                // it was reconstructed from, so this arm mirrors the planned
                // writer's handling without being reachable from that planner.
                None => None,
            }
        };
        let offset = self.bytes.len();
        self.bytes
            .extend_from_slice(format!("{} {} obj\n", output.number, output.generation).as_bytes());
        if let Some(ctx) = self.encryption_context {
            let (_, stream_data) = object_streams::wrap_objstm_body_as_handle(
                &body,
                self.options.compress_streams,
                extends,
            )?; // cov:ignore: LLVM attributes the covered encrypted ObjStm wrapper call terminator to callback cleanup.
            let mut stream_length = stream_data.len();
            crate::writer::adjust_aes_stream_length(&mut stream_length, ctx, true)?;
            crate::writer::write_objstm_dictionary(
                self.bytes,
                stream_length,
                matches!(self.options.compress_streams, CompressStreams::Yes),
                body.n_members,
                body.first_offset,
                extends,
            );
            crate::writer::write_stream_payload_with_pipeline(
                self.bytes,
                &stream_data,
                self.options.newline_before_endstream,
                output,
                ctx,
                true,
                None,
            )?; // cov:ignore: LLVM attributes the covered encrypted ObjStm pipeline call terminator to callback cleanup.
        } else {
            serialize::write_objstm_stream_with_extends(
                self.bytes,
                &body,
                self.options.compress_streams,
                self.options.newline_before_endstream,
                extends,
            )?; // cov:ignore: error arm requires an in-memory zlib encoder failure
        }
        self.bytes.extend_from_slice(b"\nendobj\n");
        self.layout
            .uncompressed
            .insert(output.number, (output.generation, offset));
        for (index, (member_output, _)) in handles.iter().enumerate() {
            self.layout.compressed.insert(
                member_output.number,
                CompressedLocation {
                    container: output.number,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                },
            );
        }
        Ok(())
    }
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

    fn qdf_object_info(&self, object: ObjectRef) -> Option<QdfObjectInfo> {
        self.options.qdf.then(|| QdfObjectInfo {
            page_sequence: self.page_sequences.get(&object).copied(),
            contents_sequence: self.contents_sequences.get(&object).copied(),
            suppress_original_object_ids: self.options.no_original_object_ids,
            original_object_id: Some(self.pdf.writer_original_object_ref(object)),
        })
    }

    fn indirect_stream_length(&self) -> Option<IndirectStreamLength> {
        self.current_stream_length
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
        self.emit_source_from_handle(object)
    }
}

impl<R: Read + Seek + 'static> PlainObjectEmitter<'_, R> {
    fn emit_source_from_handle(&mut self, handle: &ObjectHandle) -> crate::Result<()> {
        self.emit_source_from_handle_with_source(handle, None)
    }

    fn emit_source_from_handle_with_source(
        &mut self,
        handle: &ObjectHandle,
        source_override: Option<ObjectRef>,
    ) -> crate::Result<()> {
        handle.try_dereference()?;
        let source = source_override.or_else(|| handle.object_ref());
        let map = |object_ref| {
            self.plan.new_for_original(object_ref).ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer: reference {} {} R absent from renumber map",
                    object_ref.number, object_ref.generation
                ))
            })
        };

        self.current_stream_length = None;
        if self.plan.root_source == source {
            if self.options.qdf {
                // The ADBE arbitration is not a non-QDF detail: qpdf performs
                // it inside the generic dictionary path, guarded by `is_root`
                // rather than by mode, and its own trace point passes
                // `m->qdf_mode ? 0 : 1` precisely because the branch runs in
                // both (`QPDFWriter.cc:1396-1436`). Serialize the arbitrated
                // copy with the QDF layout instead of skipping arbitration.
                // `true`: this is the indirect Catalog, which is `is_root` for
                // qpdf (`old_og == m->root_og`, `QPDFWriter.cc:1374`).
                let arbitrated = handle.output_root_copy_with_adbe(
                    &self.plan.version,
                    self.plan.final_extension_level,
                    true,
                )?; // cov:ignore: LLVM attributes this covered multiline call terminator to the call setup
                return arbitrated.write_object_qdf_with_ref_map_and_removed(
                    self.bytes,
                    0,
                    &map,
                    &self.plan.removed_refs,
                );
            }
            return handle.write_root_object_with_ref_map_and_removed(
                self.bytes,
                &map,
                &self.plan.removed_refs,
                &self.plan.version,
                self.plan.final_extension_level,
                true,
            );
        }

        if handle.as_stream_dict().is_some() {
            let cached = if let Some(source) = source {
                if let Some(cached) = self.plan.cached_stream_outputs.get(&source) {
                    if cached.fingerprint == stream_cache_fingerprint(handle)? {
                        Some((
                            cached.dict.clone(),
                            cached.data.clone(),
                            cached.dictionary_options,
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None // cov:ignore: writer emission only receives indirect stream handles with a source identity
            };
            let (dict, data, dictionary_options) = match cached {
                Some(cached) => cached,
                None => crate::writer::plain::body::canonical_stream_output_for_rewrite(
                    handle,
                    self.options,
                    source.is_some_and(|source| {
                        self.options.content_normalization
                            && self.contents_sequences.contains_key(&source)
                    }),
                )?, // cov:ignore: LLVM maps this validated fallback continuation to the error arm
            };
            if self.options.qdf {
                // cov:ignore-start: `write_object` supplies only indirect
                // planned streams, so every QDF stream has a source identity.
                let stream_number = self.output_number(source.ok_or_else(|| {
                    crate::Error::Unsupported("plain writer stream lost its source identity".into())
                })?)?;
                // cov:ignore-end
                let holder = self
                    .plan
                    .qdf_holder_map
                    .get(&stream_number)
                    .copied()
                    .map(|number| ObjectRef::new(number, 0))
                    .ok_or_else(|| {
                        // cov:ignore-start: qdf_holder_map is built from the
                        // same planned stream placements before emission.
                        crate::Error::Unsupported(format!(
                            "plain writer QDF: stream {stream_number} has no length holder"
                        ))
                        // cov:ignore-end
                    })?; // cov:ignore: qdf_holder_map is populated for every valid planned stream
                dict.write_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
                    self.bytes,
                    0,
                    &map,
                    &self.plan.removed_refs,
                    Some(holder),
                    dictionary_options,
                )?; // cov:ignore: LLVM maps this validated QDF dictionary continuation to the call setup.
                let added_newline = serialize::framing_adds_newline_with_qdf(
                    &data,
                    self.options.newline_before_endstream,
                    true,
                );
                serialize::write_stream_payload_with_qdf(
                    self.bytes,
                    &data,
                    self.options.newline_before_endstream,
                    true,
                );
                self.current_stream_length = Some(IndirectStreamLength {
                    cur_stream_length: data.len(),
                    added_newline,
                });
            } else {
                dict.write_stream_body_with_ref_map_and_removed_with_options(
                    self.bytes,
                    dictionary_options,
                    &map,
                    &self.plan.removed_refs,
                )?;
                serialize::write_stream_payload(
                    self.bytes,
                    &data,
                    self.options.newline_before_endstream,
                );
            }
        } else if self.options.qdf {
            handle.write_object_qdf_with_ref_map_and_removed(
                self.bytes,
                0,
                &map,
                &self.plan.removed_refs,
            )?; // cov:ignore: the shared QDF handle serializer is covered by its own contract tests.
        } else {
            handle.write_object_with_ref_map_and_removed(
                self.bytes,
                &map,
                &self.plan.removed_refs,
            )?; // cov:ignore: the shared compact handle serializer is covered by its own contract tests.
        }
        Ok(())
    }

    fn emit_raw_source(
        &mut self,
        raw: crate::qpdf_obj_gen::QpdfObjGen,
        output: ObjectRef,
    ) -> crate::Result<()> {
        let handle = self
            .pdf
            .get_object_handle_by_raw_identity(raw.get_obj() as i32, raw.get_gen() as i32);
        self.indicate_progress()?;
        // cov:ignore-start: qdf raw-generation provenance has no valid
        // qdf fixture because qpdf rejects an out-of-range header in this
        // writer route; the default writer path is covered below.
        if self.options.qdf && !self.options.no_original_object_ids {
            self.bytes.extend_from_slice(
                format!(
                    "%% Original object ID: {} {}\n",
                    raw.get_obj(),
                    raw.get_gen()
                )
                .as_bytes(),
            );
        }
        // cov:ignore-end
        self.open_object(output.number)?;
        self.encryption.set_data_key(output.number);
        self.emit_source_from_handle_with_source(&handle, Some(output))?;
        self.encryption.clear_data_key();
        self.close_object(output.number, self.options.qdf)?;
        // cov:ignore-start: qdf raw-stream length-holder framing is
        // unavailable for the malformed raw-generation fixture.
        if let Some(length) = self.current_stream_length.take() {
            if handle.as_stream_dict().is_some() {
                if self.options.qdf && length.added_newline {
                    self.bytes.extend_from_slice(b"%QDF: ignore_newline\n");
                }
                let holder = self
                    .plan
                    .qdf_holder_map
                    .get(&output.number)
                    .copied()
                    .ok_or_else(|| {
                        crate::Error::Unsupported(
                            "plain writer QDF: raw stream has no length holder".into(),
                        )
                    })?;
                self.open_object(holder)?;
                self.bytes
                    .extend_from_slice(length.cur_stream_length.to_string().as_bytes());
                self.close_object(holder, self.options.qdf)?;
            }
        }
        // cov:ignore-end
        Ok(())
    }

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
            handles.push((member.output, handle));
        }
        let mut qdf_marker_starts = Vec::new();
        let mut qdf_marker_lengths = Vec::new();
        let map = |object_ref| {
            plan.new_for_original(object_ref).ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain writer: reference {} {} R absent from renumber map",
                    object_ref.number, object_ref.generation
                ))
            })
        };
        let body_writer = &mut |out: &mut Vec<u8>,
                                member_index: u32,
                                member_ref: ObjectRef,
                                handle: &ObjectHandle|
         -> crate::Result<()> {
            if options.qdf {
                let marker_start = out.len();
                let source_member = usize::try_from(member_index)
                    .ok()
                    .and_then(|index| members.get(index))
                    .map(|member| member.source)
                    .unwrap_or(member_ref);
                let original = pdf.writer_original_object_ref(source_member);
                out.extend_from_slice(
                    format!(
                        "%% Object stream: object {}, index {}",
                        member_ref.number, member_index
                    )
                    .as_bytes(),
                );
                // cov:ignore-start: the generation arm below is unreachable --
                // qpdf writes a generation only when it is non-zero
                // (`QPDFWriter.cc:1697-1702`) and ISO 32000-1 7.5.7 requires
                // every object in an object stream to have generation 0. The
                // block is wrapped whole because llvm-cov attributes the
                // uncovered region to the enclosing closing brace.
                if !options.no_original_object_ids {
                    out.extend_from_slice(
                        format!("; original object ID: {}", original.number).as_bytes(),
                    );
                    if original.generation != 0 {
                        out.extend_from_slice(format!(" {}", original.generation).as_bytes());
                    }
                }
                // cov:ignore-end
                out.push(b'\n');
                qdf_marker_starts.push(marker_start);
                qdf_marker_lengths.push(out.len() - marker_start);
                if let Some(sequence) = self.page_sequences.get(&source_member) {
                    out.extend_from_slice(format!("%% Page {sequence}\n").as_bytes());
                }
            }
            handle.try_dereference()?;
            let is_root = handle.object_ref() == plan.root_source;
            let handle_to_write = if handle.as_stream_dict().is_some() {
                // qpdf's `writeObjectStream` warns through the member handle
                // and substitutes an indirect null before serialization
                // (`QPDFWriter.cc:1690-1705`). Preserve the same policy on
                // the planned source-backed ObjStm route.
                // See the live writer above: qpdf warns once in each of its
                // two ObjStm passes.
                // cov:ignore-start: qtest fuzz-16214 is the corpus-level stream-in-ObjStm warning oracle
                for _ in 0..2 {
                    handle
                        .warn_if_possible("stream found inside object stream; treating as null")?;
                }
                ObjectHandle::null()
                // cov:ignore-end
            } else {
                handle.clone()
            };
            let result = if options.qdf {
                // A Catalog compressed into an ObjStm is still the root, and
                // qpdf's ADBE arbitration keys on `is_root` rather than on the
                // output mode (`QPDFWriter.cc:1396-1436`), so it applies here
                // exactly as it does to an uncompressed root.
                if is_root {
                    let arbitrated = handle_to_write.output_root_copy_with_adbe(
                        &plan.version,
                        plan.final_extension_level,
                        true,
                    )?; // cov:ignore: LLVM attributes this covered multiline call terminator to the call setup
                    arbitrated.write_object_qdf_with_ref_map_and_removed(
                        out,
                        0,
                        &map,
                        &plan.removed_refs,
                    )
                } else {
                    handle_to_write.write_object_qdf_with_ref_map_and_removed(
                        out,
                        0,
                        &map,
                        &plan.removed_refs,
                    )
                }
            } else if is_root {
                handle_to_write.write_root_object_with_ref_map_and_removed(
                    out,
                    &map,
                    &plan.removed_refs,
                    &plan.version,
                    plan.final_extension_level,
                    true,
                )
            } else {
                handle_to_write.write_object_with_ref_map_and_removed(out, &map, &plan.removed_refs)
            };
            if result.is_ok() {
                crate::writer::report_progress_event(options)?;
            }
            result
        };
        let body = if options.qdf {
            object_streams::emit_objstm_body_from_handles_with_writer_qdf(&handles, body_writer)?
        } else {
            object_streams::emit_objstm_body_from_handles_with_writer(&handles, body_writer)?
        };
        let mut body = body;
        let mut qdf_first_offset = None;
        if options.qdf {
            let first_marker_len = qdf_marker_lengths.first().copied().ok_or_else(|| {
                // cov:ignore-start: every valid ObjStm placement contains at
                // least one member and therefore records one marker.
                crate::Error::Internal("plain writer QDF marker lengths are empty".to_string())
                // cov:ignore-end
            })?; // cov:ignore: every valid ObjStm placement records at least one QDF marker
            let objects_section = body.bytes.split_off(body.first_offset);
            let mut pair_table = Vec::new();
            for (index, ((member, _), (&marker_start, &marker_len))) in handles
                .iter()
                .zip(qdf_marker_starts.iter().zip(qdf_marker_lengths.iter()))
                .enumerate()
            {
                if index != 0 {
                    pair_table.push(b'\n');
                }
                let offset = marker_start
                    .checked_add(marker_len)
                    .and_then(|end| end.checked_sub(first_marker_len))
                    .ok_or_else(|| {
                        // cov:ignore-start: marker positions and lengths are
                        // derived from one allocatable in-memory Vec.
                        crate::Error::Unsupported(
                            "plain writer QDF ObjStm member offset overflows usize".to_string(),
                        )
                        // cov:ignore-end
                    })?; // cov:ignore: marker arithmetic is bounded by one allocatable in-memory Vec
                use std::io::Write as _;
                let _ = write!(pair_table, "{} {}", member.number, offset);
            }
            pair_table.push(b'\n');
            body.first_offset = pair_table.len();
            pair_table.extend_from_slice(&objects_section);
            body.bytes = pair_table;
            qdf_first_offset = Some(body.first_offset.checked_add(first_marker_len).ok_or_else(
                // cov:ignore-start: both operands are lengths of one
                // allocatable in-memory ObjStm body.
                || crate::Error::Unsupported("plain writer QDF /First overflows usize".to_string()),
                // cov:ignore-end
            )?); // cov:ignore: QDF /First arithmetic is bounded by one allocatable in-memory Vec
        }
        let offset = bytes.len();
        bytes
            .extend_from_slice(format!("{} {} obj\n", output.number, output.generation).as_bytes());
        let structural_compress = if options.qdf {
            CompressStreams::No
        } else if plan.trailer.structural_filtered {
            CompressStreams::Yes
        } else {
            CompressStreams::No
        };
        let extends = match origin {
            crate::writer::plain::plan::PlannedObjectStreamOrigin::SourceBacked(source) => {
                let source_handle = pdf.get_object_handle(*source);
                source_handle.try_dereference()?;
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
            crate::writer::plain::plan::PlannedObjectStreamOrigin::Generated(_) => None,
            crate::writer::plain::plan::PlannedObjectStreamOrigin::Synthetic => None,
        };
        if options.qdf {
            serialize::write_objstm_stream_with_extends_qdf(
                bytes,
                &body,
                extends,
                qdf_first_offset.expect("QDF first offset is set with QDF body"),
                options.newline_before_endstream,
            )?; // cov:ignore: the QDF ObjStm wrapper is exercised by byte-parity tests; LLVM maps this validated continuation to the call setup.
        } else {
            serialize::write_objstm_stream_with_extends(
                bytes,
                &body,
                structural_compress,
                options.newline_before_endstream,
                extends,
            )?; // cov:ignore: error arm requires an in-memory zlib encoder failure
        }
        bytes.extend_from_slice(b"\nendobj\n");
        if options.qdf {
            bytes.push(b'\n');
        }
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
                    if value.try_is_array()? && has_direct_stream_in_value(value)? {
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
    // The output dictionary is a shallow writer copy. Preserve the source
    // stream's warning context on that copy so qpdf's missing-key
    // `/DecodeParms` erase boundary remains observable when `/Filter` carries
    // `/Crypt` without a paired parameters entry.
    if dict.context().is_none() && handle.context().is_some() {
        dict.set_child_description(handle, b" -> stream dictionary", b"");
    }
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
            member_handle.try_dereference()?;
            let is_signature = object_streams::is_qpdf_signature_dict(&member_handle)?;
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
    use super::{
        canonical_stream_output_for_rewrite, emit_content_container_from_handle_with_ref_map,
    };
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

    #[test]
    fn rewrite_wrapper_returns_the_shared_stream_dictionary_policy() {
        let pdf = crate::Pdf::empty().unwrap();
        let stream = pdf
            .new_stream_with_data(Rc::new(b"q Q\n".to_vec()))
            .unwrap();
        let (dictionary, data, policy) =
            canonical_stream_output_for_rewrite(&stream, &WriterOptions::default(), false)
                .expect("canonical rewrite stream output");
        assert!(!data.is_empty());
        assert_eq!(
            dictionary.try_get_key(b"/Length").unwrap().as_integer(),
            Some(data.len() as i64)
        );
        assert!(policy.add_flate_filter);
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
        with_emitter_options(pdf, plan, WriterOptions::default(), check);
    }

    fn with_emitter_options(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        plan: &PlainWritePlan,
        options: WriterOptions,
        check: impl FnOnce(&mut PlainObjectEmitter<'_, Cursor<Vec<u8>>>),
    ) {
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
            page_sequences: BTreeMap::new(),
            contents_sequences: BTreeMap::new(),
            current_stream_length: None,
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

    fn pdf_with_indirect_stream() -> (Pdf<Cursor<Vec<u8>>>, ObjectHandle) {
        let mut pdf = Pdf::empty().unwrap();
        let stream = pdf
            .new_stream_with_data(Rc::new(b"stream-data".to_vec()))
            .unwrap();
        pdf.root_handle()
            .unwrap()
            .replace_key(b"Stream", stream.clone())
            .unwrap();
        (pdf, stream)
    }

    #[test]
    fn specialized_live_object_stream_replaces_a_stream_member_with_null() -> crate::Result<()> {
        // A valid ObjStm cannot contain a stream body, but a recovered source
        // membership can still expose that malformed shape to the writer. qpdf
        // warns once per pass and serializes the member as an indirect null
        // (`QPDFWriter.cc:1690-1705`); keep the specialized live consumer's
        // defensive boundary covered directly.
        let mut pdf = Pdf::empty()?;
        let container = pdf.new_stream_with_data(Rc::new(Vec::new()))?;
        let member = pdf.new_stream_with_data(Rc::new(b"not-an-objstm-member".to_vec()))?;
        pdf.root_handle()?
            .replace_key(b"/MalformedObjStm", container.clone())?;
        let container_source = container
            .object_ref()
            .expect("synthetic ObjStm container is indirect");
        let member_source = member.object_ref().expect("synthetic member is indirect");
        let groups = [object_streams::ObjectStreamGroup::SourceBacked {
            source: container_source,
            members: vec![member_source],
        }];
        let options = WriterOptions {
            object_streams: crate::writer::ObjectStreamMode::Preserve,
            compress_streams: CompressStreams::No,
            extra_header_text: "% malformed-objstm-member\n".to_string(),
            static_id: true,
            ..WriterOptions::default()
        };
        let root_source = pdf.root_ref();
        let body = emit_live_specialized_standard(
            &mut pdf,
            &options,
            "1.5",
            0,
            root_source,
            BTreeSet::new(),
            &groups,
            None,
        )?; // cov:ignore: the malformed-member live-body test covers this validated call; LLVM attributes the multiline continuation here.
        assert!(
            body.bytes
                .windows(b"null".len())
                .any(|window| window == b"null"),
            "malformed ObjStm stream members must be emitted as null"
        );
        Ok(())
    }

    #[test]
    fn a_new_source_missing_from_the_frozen_plan_fails_before_open_object() {
        // The existing planner backend remains frozen until the live queue
        // cutover. Its lookup error must cross the shared owner unchanged.
        let mut pdf = super::object_emitter_tests::pdf();
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
    fn source_object_mapping_errors_propagate_from_the_plain_unparser() {
        let mut pdf = pdf();
        let child = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(7))
            .unwrap();
        let child_id = child.object_ref().unwrap();
        let object = pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/Child".to_vec(),
                child,
            )]))
            .unwrap();
        let object_id = object.object_ref().unwrap();
        pdf.root_handle()
            .unwrap()
            .replace_key(b"Object", object.clone())
            .unwrap();
        let mut plan = PlainWritePlan::build(&mut pdf, &WriterOptions::default()).unwrap();
        plan.old_to_new.remove(&child_id);

        with_emitter(&mut pdf, &plan, |emitter| {
            let error = emitter
                .write_object(&object, None)
                .expect_err("an unmapped child must fail during unparse");
            assert!(matches!(error, crate::Error::Unsupported(message)
                if message.contains("absent from renumber map")));
            assert!(emitter
                .layout
                .uncompressed
                .contains_key(&plan.new_for_original(object_id).unwrap().number));
        });
    }

    #[test]
    fn source_stream_recomputes_when_the_planned_cache_is_missing_or_stale() {
        for stale in [false, true] {
            let (mut pdf, stream) = pdf_with_indirect_stream();
            let mut plan = PlainWritePlan::build(&mut pdf, &WriterOptions::default()).unwrap();
            if stale {
                stream.replace_stream_data(Rc::new(b"changed-data".to_vec()), None, None);
            } else {
                plan.cached_stream_outputs.clear();
            }
            with_emitter(&mut pdf, &plan, |emitter| {
                emitter
                    .write_object(&stream, None)
                    .expect("a stream without a usable cache must be recomputed");
                assert!(emitter
                    .bytes
                    .windows(b"endstream".len())
                    .any(|window| window == b"endstream"));
            });
        }

        let (mut pdf, stream) = pdf_with_indirect_stream();
        let options = WriterOptions {
            content_normalization: true,
            ..WriterOptions::default()
        };
        let mut plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        plan.cached_stream_outputs.clear();
        let stream_id = stream.object_ref().unwrap();
        with_emitter_options(&mut pdf, &plan, options, |emitter| {
            emitter.contents_sequences.insert(stream_id, 1);
            emitter
                .write_object(&stream, None)
                .expect("a normalized stream without a cache must be recomputed");
            assert!(emitter
                .bytes
                .windows(b"endstream".len())
                .any(|window| window == b"endstream"));
        });
    }

    #[test]
    fn qdf_source_stream_uses_the_planned_holder_and_synthetic_objstm_uses_qdf_wrapper() {
        let (mut pdf, stream) = pdf_with_indirect_stream();
        let options = WriterOptions {
            qdf: true,
            ..WriterOptions::default()
        };
        let plan = PlainWritePlan::build(&mut pdf, &options).unwrap();
        let stream_id = stream.object_ref().unwrap();
        let stream_output = plan.new_for_original(stream_id).unwrap();
        assert!(plan.qdf_holder_map.contains_key(&stream_output.number));

        with_emitter_options(&mut pdf, &plan, options, |emitter| {
            emitter
                .write_object(&stream, None)
                .expect("QDF stream emission");
            assert!(emitter
                .bytes
                .windows(b"/Length ".len())
                .any(|window| { window == b"/Length " }));
        });

        let member = pdf.get_object_handle(ObjectRef::new(2, 0));
        let member_id = member.object_ref().unwrap();
        let members = [PlannedMember {
            source: member_id,
            output: plan.new_for_original(member_id).unwrap(),
        }];
        pdf.set_writer_object_order(BTreeMap::from([(
            member_id,
            crate::pdf::WriterObjectOrderKey::foreign_with_allocation_identity(
                member_id,
                ObjectRef::new(12, 1),
            ),
        )]));
        let synthetic_options = WriterOptions {
            qdf: true,
            ..WriterOptions::default()
        };
        with_emitter_options(&mut pdf, &plan, synthetic_options, |emitter| {
            emitter
                .emit_planned_object_stream(
                    &PlannedObjectStreamOrigin::Synthetic,
                    ObjectRef::new(20, 0),
                    &members,
                )
                .expect("synthetic QDF ObjStm emission");
            assert!(emitter
                .bytes
                .windows(b"/Type /ObjStm".len())
                .any(|window| window == b"/Type /ObjStm"));
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

    #[test]
    fn live_queue_checks_owner_deduplicates_and_walks_direct_seed_containers() -> crate::Result<()>
    {
        let mut local_pdf = super::object_emitter_tests::pdf();
        let mut foreign_pdf = super::object_emitter_tests::pdf();
        let foreign = foreign_pdf
            .make_indirect_object_handle(ObjectHandle::integer(1))
            .unwrap();
        let mut queue = LiveQueue::new(BTreeSet::new());
        let error = queue
            .enqueue_handle(&mut local_pdf, foreign)
            .expect_err("foreign live handles must be rejected");
        assert!(error.to_string().contains("different QPDF"));

        let child = local_pdf
            .make_indirect_object_handle(ObjectHandle::integer(42))
            .unwrap();
        let child_ref = child.object_ref().unwrap();
        let mut removed_queue = LiveQueue::new([child_ref].into_iter().collect());
        assert_eq!(
            removed_queue.enqueue_handle(&mut local_pdf, child.clone())?,
            None
        );

        let output_ref = queue.enqueue_handle(&mut local_pdf, child.clone())?;
        assert_eq!(output_ref, Some(ObjectRef::new(1, 0)));
        assert_eq!(
            queue.enqueue_handle(&mut local_pdf, child.clone())?,
            output_ref
        );
        assert_eq!(
            queue.pop().and_then(|handle| handle.object_ref()),
            Some(child_ref)
        );
        assert!(queue.pop().is_none());

        let direct_array = ObjectHandle::array(vec![child.clone(), ObjectHandle::null()]);
        let mut seeds = Vec::new();
        collect_live_seed_handles(&direct_array, &mut seeds, 0)?;
        assert_eq!(seeds.len(), 1);
        assert!(seeds[0].is_same_object_as(&child));
        let direct_dictionary = ObjectHandle::dictionary(vec![
            (b"/Child".to_vec(), child.clone()),
            (b"/Null".to_vec(), ObjectHandle::null()),
        ]);
        seeds.clear();
        collect_live_seed_handles(&direct_dictionary, &mut seeds, 0)?;
        assert_eq!(seeds.len(), 1);
        assert!(seeds[0].is_same_object_as(&child));

        let direct_stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Child".to_vec(), child.clone())]),
            Rc::new(b"seed".to_vec()),
        );
        seeds.clear();
        collect_live_seed_handles(&direct_stream, &mut seeds, 0)?;
        assert_eq!(seeds.len(), 1);
        assert!(seeds[0].is_same_object_as(&child));

        local_pdf
            .trailer()
            .replace_key(b"/DecodeParms", ObjectHandle::integer(1))?;
        let root_source = local_pdf.root_ref();
        let body = emit_live_disable(
            &mut local_pdf,
            &WriterOptions::default(),
            "1.4",
            0,
            root_source,
            BTreeSet::new(),
            &[],
        )?; // cov:ignore: LLVM attributes the live-body test call terminator to callback cleanup.
        assert!(!body.bytes.is_empty());
        Ok(())
    }

    #[test]
    fn enqueue_ignores_an_object_stream_that_contains_itself() -> crate::Result<()> {
        // A specially constructed file can name a container that is itself a
        // member. qpdf stores the invalid object ID `0` before recursing and
        // ignores the object when it meets that sentinel again
        // (`libqpdf/QPDFWriter.cc:1097-1104,1120-1124`), dropping the looping
        // object instead of recursing forever.
        let container = ObjectRef::new(5, 0);
        let mut pdf = super::object_emitter_tests::pdf();
        let mut queue = LiveQueue::new(BTreeSet::new());
        queue.register_object_streams(&[object_streams::ObjectStreamGroup::SourceBacked {
            source: container,
            members: vec![container],
        }]);

        let handle = pdf.get_object_handle(container);
        assert_eq!(queue.enqueue_handle(&mut pdf, handle)?, None);
        assert!(queue.old_to_new.is_empty());

        // A two-container cycle takes the same path one level deeper.
        let other = ObjectRef::new(6, 0);
        let mut queue = LiveQueue::new(BTreeSet::new());
        queue.register_object_streams(&[
            object_streams::ObjectStreamGroup::SourceBacked {
                source: container,
                members: vec![other],
            },
            object_streams::ObjectStreamGroup::SourceBacked {
                source: other,
                members: vec![container],
            },
        ]);
        let handle = pdf.get_object_handle(container);
        assert_eq!(queue.enqueue_handle(&mut pdf, handle)?, None);
        assert!(queue.old_to_new.is_empty());
        Ok(())
    }

    #[test]
    fn register_object_streams_skips_a_group_whose_members_are_all_removed() {
        let source = ObjectRef::new(3, 0);
        let member = ObjectRef::new(2, 0);
        let mut queue = LiveQueue::new([member].into_iter().collect());
        queue.register_object_streams(&[object_streams::ObjectStreamGroup::SourceBacked {
            source,
            members: vec![member],
        }]);
        assert!(queue.member_to_container.is_empty());
        assert!(queue.container_to_members.is_empty());
    }

    #[test]
    fn removed_source_container_still_numbers_retained_members() -> crate::Result<()> {
        let container = ObjectRef::new(3, 0);
        let member = ObjectRef::new(2, 0);
        let mut pdf = super::object_emitter_tests::pdf();
        let mut queue = LiveQueue::new([container].into_iter().collect());
        queue.register_object_streams(&[object_streams::ObjectStreamGroup::SourceBacked {
            source: container,
            members: vec![member],
        }]);

        let handle = pdf.get_object_handle(container);
        let output = queue
            .enqueue_handle(&mut pdf, handle)?
            .expect("qpdf enqueues a removed ObjStm container");
        assert_eq!(output, ObjectRef::new(1, 0));
        assert_eq!(
            queue.old_to_new.get(&container),
            Some(&ObjectRef::new(1, 0))
        );
        assert_eq!(queue.old_to_new.get(&member), Some(&ObjectRef::new(2, 0)));
        Ok(())
    }

    #[test]
    fn live_object_stream_with_a_non_stream_source_omits_extends() -> crate::Result<()> {
        let mut pdf = super::object_emitter_tests::pdf();
        let source = pdf.make_indirect_from_object_handle(ObjectHandle::null())?;
        let source_id = source.object_ref().unwrap();
        let member = pdf.make_indirect_from_object_handle(ObjectHandle::integer(42))?;
        let member_id = member.object_ref().unwrap();
        pdf.root_handle()?.replace_key(b"/Member", member)?;
        let root_source = pdf.root_ref();
        let object_streams = [object_streams::ObjectStreamGroup::SourceBacked {
            source: source_id,
            members: vec![member_id],
        }];
        let body = emit_live_disable(
            &mut pdf,
            &WriterOptions::default(),
            "1.5",
            0,
            root_source,
            BTreeSet::new(),
            &object_streams,
        )?; // cov:ignore: LLVM attributes the live-body test call terminator to callback cleanup.
        assert!(body
            .bytes
            .windows(b"/Type /ObjStm".len())
            .any(|window| window == b"/Type /ObjStm"));
        assert!(!body
            .bytes
            .windows(b"/Extends".len())
            .any(|window| window == b"/Extends"));
        Ok(())
    }

    #[test]
    fn live_object_stream_extends_an_unreferenced_predecessor_through_live_enqueue(
    ) -> crate::Result<()> {
        let mut pdf = super::object_emitter_tests::pdf();
        let predecessor = pdf.new_stream_with_data(Rc::new(Vec::new()))?;
        let predecessor_id = predecessor.object_ref().unwrap();
        let source = pdf.new_stream_with_data(Rc::new(Vec::new()))?;
        let source_id = source.object_ref().unwrap();
        source
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Extends", predecessor)?;
        let member = pdf.make_indirect_from_object_handle(ObjectHandle::integer(7))?;
        let member_id = member.object_ref().unwrap();
        pdf.root_handle()?.replace_key(b"/Member", member)?;
        let root_source = pdf.root_ref();
        let object_streams = [object_streams::ObjectStreamGroup::SourceBacked {
            source: source_id,
            members: vec![member_id],
        }];
        let body = emit_live_disable(
            &mut pdf,
            &WriterOptions::default(),
            "1.5",
            0,
            root_source,
            BTreeSet::new(),
            &object_streams,
        )?; // cov:ignore: LLVM attributes the live-body test call terminator to callback cleanup.
        let predecessor_output = body
            .old_to_new
            .get(&predecessor_id)
            .expect("an unreferenced /Extends predecessor is still discovered and numbered");
        let expected = format!("/Extends {} 0 R", predecessor_output.number);
        assert!(body
            .bytes
            .windows(expected.len())
            .any(|window| window == expected.as_bytes()));
        Ok(())
    }
}
