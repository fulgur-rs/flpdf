//! qpdf correspondence: QPDFWriter.cc plain object-body emission split from planning and xref output.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;

use crate::qpdf_obj_gen::QpdfObjGen;
use crate::writer::object_streams;
use crate::writer::output::OutputSink;
#[cfg(test)]
use crate::writer::plain::plan::{
    PlainWritePlan, PlannedIndirectObject, PlannedObjectStreamOrigin,
};
use crate::writer::plain::xref::{BodyLayout, CompressedLocation};
use crate::writer::write_object::{IndirectStreamLength, QdfObjectInfo, WriteObject};
use crate::writer::WriterOptions;
use crate::writer::{
    serialize, CompressStreams, ObjectWriterEmission, StreamDictionaryOptions, QPDF_BINARY_MARKER,
};
use crate::{ObjectHandle, ObjectRef, PageDocumentHelper, Pdf};

/// The qpdf standard-writer queue for plain Disable, Preserve, Generate, and
/// QDF output. [`LiveQueue::register_object_streams`] installs any setup-time
/// container membership before the walk. Numbers are assigned when a reference
/// is first observed, and the pending queue may grow while an object is being
/// unparsed. This is the Rust counterpart of `QPDFWriter::enqueueObject` plus
/// `object_queue`.
struct LiveQueue {
    next_objid: u32,
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
    /// QDF source stream -> reserved output-only indirect `/Length` holder.
    stream_length_holders: BTreeMap<ObjectRef, ObjectRef>,
    /// Generated ObjStm sources are writer-owned placeholders, not
    /// preserve-unreferenced seeds.
    generated_container_sources: BTreeSet<ObjectRef>,
    qdf: bool,
}

impl LiveQueue {
    /// Allocate the next output object number.
    ///
    /// Both maps name objects in the same output number space, so the counter
    /// has to span them: numbering the ordinary map alone hands a number that a
    /// raw-identity object already holds, and the later xref entry overwrites
    /// the earlier one, dropping an object from the file.
    fn reserve_output_number(&mut self) -> crate::Result<ObjectRef> {
        let number = self.next_objid;
        self.next_objid = self.next_objid.checked_add(1).ok_or_else(|| {
            crate::Error::Unsupported("plain live writer object number overflows u32".into())
        })?;
        Ok(ObjectRef::new(number, 0))
    }

    fn new(removed_refs: BTreeSet<ObjectRef>, qdf: bool) -> Self {
        Self {
            next_objid: 1,
            old_to_new: BTreeMap::new(),
            raw_old_to_new: BTreeMap::new(),
            pending: VecDeque::new(),
            removed_refs,
            member_to_container: BTreeMap::new(),
            container_to_members: BTreeMap::new(),
            resolving_members: BTreeSet::new(),
            stream_length_holders: BTreeMap::new(),
            generated_container_sources: BTreeSet::new(),
            qdf,
        }
    }

    /// Register qpdf's source-backed or generated ObjStm membership so that
    /// [`Self::enqueue_handle`] redirects a member's discovery to its
    /// container instead of numbering the member as a plain indirect object,
    /// matching `QPDFWriter::enqueueObject`'s member branch
    /// (`libqpdf/QPDFWriter.cc:1071-1132`).
    fn register_object_streams<R: Read + Seek>(
        &mut self,
        pdf: &Pdf<R>,
        groups: &[crate::writer::object_streams::ObjectStreamGroup],
    ) -> crate::Result<()> {
        for group in groups {
            let (source, members, generated) = match group {
                crate::writer::object_streams::ObjectStreamGroup::SourceBacked {
                    source,
                    members,
                } => (*source, members, false),
                crate::writer::object_streams::ObjectStreamGroup::Generated { source, members } => {
                    (*source, members, true)
                }
                crate::writer::object_streams::ObjectStreamGroup::Synthetic { .. } => {
                    return Err(crate::Error::Internal(
                        "plain live writer received an ObjStm group without source identity".into(),
                    ));
                }
            };
            if generated {
                self.generated_container_sources.insert(source);
            }
            // qpdf excludes a removed/ineligible member from
            // `object_to_object_stream` before the enqueue walk begins
            // (`preserveObjectStreams`, `libqpdf/QPDFWriter.cc:1957-1966`),
            // so it is never treated as a compressed member at all. Mirror
            // that exclusion here rather than letting a later removed-ref
            // check race the eager numbering loop in `enqueue_handle`.
            let mut retained: Vec<ObjectRef> = members
                .iter()
                .copied()
                .filter(|member| !self.removed_refs.contains(member))
                .collect();
            if pdf.writer_object_order.is_some() {
                retained.sort_unstable_by_key(|member| pdf.writer_object_order_key(*member));
            } else {
                retained.sort_unstable_by_key(|member| (member.number, member.generation));
            }
            if retained.is_empty() {
                continue;
            }
            for member in &retained {
                self.member_to_container.insert(*member, source);
            }
            self.container_to_members.insert(source, retained);
        }
        Ok(())
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
        if self.qdf && handle.try_is_stream_of_type(b"XRef", b"")? {
            return Ok(None);
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
            let output = self.reserve_output_number()?;
            self.raw_old_to_new.insert(raw_source, output);
            self.pending.push_back(handle);
            if self.qdf
                && self
                    .pending
                    .back()
                    .is_some_and(|handle| handle.as_stream_dict().is_some())
            {
                let holder = self.reserve_output_number()?;
                self.stream_length_holders
                    .insert(ObjectRef::new(output.number, 0), holder);
            }
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
        let output = self.reserve_output_number()?;
        self.old_to_new.insert(source, output);
        self.pending.push_back(handle);
        if let Some(members) = self.container_to_members.get(&source).cloned() {
            for member in members {
                if !self.old_to_new.contains_key(&member) {
                    let member_output = self.reserve_output_number()?;
                    self.old_to_new.insert(member, member_output);
                }
            }
        } else if self.qdf
            && self
                .pending
                .back()
                .is_some_and(|handle| handle.as_stream_dict().is_some())
        {
            let holder = self.reserve_output_number()?;
            self.stream_length_holders.insert(source, holder);
        }
        Ok(Some(output))
    }

    fn pop(&mut self) -> Option<ObjectHandle> {
        self.pending.pop_front()
    }
}

/// Layout and numbering metadata produced by the live body pass. Final PDF
/// bytes remain owned by the configured [`OutputSink`].
pub(crate) struct LiveBodyOutput {
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

/// Collect the children qpdf's `unparseObject` will hand to `unparseChild`
/// from the value currently being emitted. Unlike seed collection, the
/// current indirect handle is deliberately opened rather than re-enqueued.
/// Dictionary children that resolve to null remain invisible, while array
/// positions retain their normal enqueue behavior.
fn collect_live_child_handles(
    handle: &ObjectHandle,
    found: &mut Vec<ObjectHandle>,
    depth: usize,
) -> crate::Result<()> {
    if depth > crate::parser::MAX_PARSE_DEPTH {
        return Err(crate::Error::Unsupported(format!(
            "plain live writer: emitted value nesting exceeds maximum of {}",
            crate::parser::MAX_PARSE_DEPTH
        )));
    }
    if let Some(items) = handle.try_as_array()? {
        for item in items {
            collect_live_seed_handles(&item, found, depth + 1)?;
        }
    } else if let Some(stream_dict) = handle.as_stream_dict() {
        for (_, value) in stream_dict.try_as_dictionary()?.unwrap_or_default() {
            if !value.try_is_null()? {
                collect_live_seed_handles(&value, found, depth + 1)?;
            }
        }
    } else if let Some(entries) = handle.try_as_dictionary()? {
        for (_, value) in entries {
            if !value.try_is_null()? {
                collect_live_seed_handles(&value, found, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn initialize_live_queue<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    removed_refs: BTreeSet<ObjectRef>,
    object_streams: &[crate::writer::object_streams::ObjectStreamGroup],
) -> crate::Result<LiveQueue> {
    let mut queue = LiveQueue::new(removed_refs, options.qdf);
    queue.register_object_streams(pdf, object_streams)?;
    if options.preserve_unreferenced_objects {
        for handle in pdf.get_all_objects()? {
            if handle
                .object_ref()
                .is_some_and(|source| queue.generated_container_sources.contains(&source))
            {
                continue;
            }
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

    Ok(queue)
}

/// Emit a plain non-linearized body using qpdf's live queue. Direct values are
/// traversed only when they are queue seeds; indirect children are discovered
/// by the writer-owned unparser while each queued object is emitted.
fn emit_live_body<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    version: &str,
    final_extension_level: i64,
    root_source: Option<ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
    object_streams: &[crate::writer::object_streams::ObjectStreamGroup],
    encryption_context: Option<&crate::writer::EncryptionContext>,
    content_container_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<LiveBodyOutput> {
    out.write_bytes(format!("%PDF-{version}\n").as_bytes())?;
    out.write_bytes(QPDF_BINARY_MARKER)?;
    let (page_sequences, contents_sequences) = if options.qdf || options.content_normalization {
        qdf_page_context(pdf)?
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };
    // qpdf initializes page/content-stream state before generating ObjStm
    // membership and before enqueueing any standard-writer seed
    // (`QPDFWriter.cc:2114-2140,2907-2925`). `getAllPages()` may repair the
    // page tree and create replacement handles, so doing this after queue
    // initialization leaves those live references without output numbers.
    let queue = initialize_live_queue(pdf, options, removed_refs.clone(), object_streams)?;
    if options.qdf {
        out.write_bytes(b"%QDF-1.0\n\n")?;
    }
    out.write_bytes(options.extra_header_text.as_bytes())?;
    let mut layout = BodyLayout::default();
    let mut emitter = LiveObjectEmitter {
        pdf,
        options,
        out,
        layout: &mut layout,
        queue: RefCell::new(queue),
        root_source,
        version,
        final_extension_level,
        removed_refs,
        lengths: BTreeMap::new(),
        encryption: crate::writer::encryption_state::WriterEncryptionState::new(
            encryption_context.is_some(),
            encryption_context
                .map(|context| context.file_key.clone())
                .unwrap_or_default(),
            encryption_context
                .is_some_and(|context| crate::writer::cipher_needs_aes_iv(context.cipher)),
            encryption_context.map_or(0, |context| context.encryption_v),
            encryption_context.map_or(0, |context| context.encryption_r),
        ),
        encrypted_strings: encryption_context
            .map(crate::writer::encrypted_strings::EncryptedStringEmitter::from_context),
        encryption_context,
        content_container_refs: content_container_refs.clone(),
        current_raw_output: None,
        page_sequences,
        contents_sequences,
        current_stream_length: None,
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
    let object_count = usize::try_from(queue.next_objid.saturating_sub(1)).map_err(|_| {
        crate::Error::Unsupported("plain live writer object count exceeds usize".into())
    })?;
    Ok(LiveBodyOutput {
        layout,
        old_to_new,
        object_count,
    })
}

pub(crate) fn emit_live<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    version: &str,
    final_extension_level: i64,
    root_source: Option<ObjectRef>,
    removed_refs: BTreeSet<ObjectRef>,
    object_streams: &[crate::writer::object_streams::ObjectStreamGroup],
    encryption_context: Option<&crate::writer::EncryptionContext>,
    content_container_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<LiveBodyOutput> {
    emit_live_body(
        pdf,
        out,
        options,
        version,
        final_extension_level,
        root_source,
        removed_refs,
        object_streams,
        encryption_context,
        content_container_refs,
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

fn map_queued_output(
    queued_map: &BTreeMap<ObjectRef, ObjectRef>,
    object: ObjectRef,
) -> crate::Result<ObjectRef> {
    queued_map.get(&object).copied().ok_or_else(|| {
        crate::Error::Unsupported(format!(
            "plain live writer: reference {} {} R absent from queue",
            object.number, object.generation
        ))
    })
}

#[cfg(test)]
fn planned_object_stream_groups(
    plan: &PlainWritePlan,
) -> crate::Result<Vec<crate::writer::object_streams::ObjectStreamGroup>> {
    let mut groups = Vec::new();
    for object in &plan.objects {
        let PlannedIndirectObject::ObjectStream {
            origin, members, ..
        } = object
        else {
            continue;
        };
        let members = members.iter().map(|member| member.source).collect();
        let group = match origin {
            PlannedObjectStreamOrigin::SourceBacked(source) => {
                crate::writer::object_streams::ObjectStreamGroup::SourceBacked {
                    source: *source,
                    members,
                }
            }
            PlannedObjectStreamOrigin::Generated(source) => {
                crate::writer::object_streams::ObjectStreamGroup::Generated {
                    source: *source,
                    members,
                }
            }
            PlannedObjectStreamOrigin::Synthetic => {
                return Err(crate::Error::Unsupported(
                    "plain live writer cannot dynamically emit a synthetic ObjStm".into(),
                ));
            }
        };
        groups.push(group);
    }
    Ok(groups)
}

/// Test-only adapter that feeds a historical placement plan into the live
/// queue while retaining final-byte ownership in the supplied `OutputSink`.
#[cfg(test)]
pub(crate) fn emit_bodies<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    plan: &PlainWritePlan,
) -> crate::Result<LiveBodyOutput> {
    validate_objstm_member_bodies(pdf, plan)?;
    let groups = planned_object_stream_groups(plan)?;
    emit_live_body(
        pdf,
        out,
        options,
        &plan.version,
        plan.final_extension_level,
        plan.root_source,
        plan.removed_refs.clone(),
        &groups,
        None,
        &BTreeSet::new(),
    )
}

struct LiveObjectEmitter<'pdf, 'output, 'sink, R: Read + Seek + 'static> {
    pdf: &'pdf mut Pdf<R>,
    options: &'pdf WriterOptions,
    out: &'output mut OutputSink<'sink>,
    layout: &'output mut BodyLayout,
    queue: RefCell<LiveQueue>,
    root_source: Option<ObjectRef>,
    version: &'pdf str,
    final_extension_level: i64,
    removed_refs: BTreeSet<ObjectRef>,
    lengths: BTreeMap<u32, usize>,
    encryption: crate::writer::encryption_state::WriterEncryptionState,
    encrypted_strings: Option<crate::writer::encrypted_strings::EncryptedStringEmitter>,
    encryption_context: Option<&'pdf crate::writer::EncryptionContext>,
    content_container_refs: BTreeSet<ObjectRef>,
    current_raw_output: Option<ObjectRef>,
    page_sequences: BTreeMap<ObjectRef, usize>,
    contents_sequences: BTreeMap<ObjectRef, usize>,
    current_stream_length: Option<IndirectStreamLength>,
}

impl<'pdf, 'output, 'sink, R: Read + Seek + 'static> crate::writer::write_object::WriteObject
    for LiveObjectEmitter<'pdf, 'output, 'sink, R>
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
        self.out.write_bytes(bytes)
    }

    fn output_count(&self) -> crate::Result<usize> {
        usize::try_from(self.out.position()).map_err(|_| {
            crate::Error::Unsupported(
                "plain live writer output position exceeds usize range".to_string(),
            )
        })
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
        self.current_stream_length = None;
        if object
            .object_ref()
            .is_some_and(|source| self.content_container_refs.contains(&source))
        {
            self.enqueue_surviving_children(object)?;
            let queued_map = self.queue.borrow().old_to_new.clone();
            let static_map = |object_ref| map_queued_output(&queued_map, object_ref);
            let output = self.output_number(object.object_ref().unwrap_or(ObjectRef::new(0, 0)))?;
            if let Some(emitter) = self.encrypted_strings.as_mut() {
                emitter.write_handle_content_container_with_ref_map(
                    self.out,
                    ObjectRef::new(output, 0),
                    None,
                    object,
                    self.options,
                    &static_map,
                    &self.removed_refs,
                )?;
            } else {
                emit_content_container_from_handle_with_ref_map(
                    object,
                    self.options,
                    self.out,
                    &static_map,
                    &self.removed_refs,
                )?;
            }
            return Ok(());
        }
        if self.root_source == object.object_ref() {
            if self.encrypted_strings.is_some() || self.options.qdf {
                let root = object.output_root_copy_with_adbe(
                    self.version,
                    self.final_extension_level,
                    self.options.qdf,
                )?;
                self.enqueue_surviving_children(&root)?;
                let queued_map = self.queue.borrow().old_to_new.clone();
                let static_map = |object_ref| map_queued_output(&queued_map, object_ref);
                let output =
                    self.output_number(object.object_ref().unwrap_or(ObjectRef::new(0, 0)))?;
                if let Some(emitter) = self.encrypted_strings.as_mut() {
                    emitter.write_handle_object_with_ref_map(
                        self.out,
                        ObjectRef::new(output, 0),
                        None,
                        &root,
                        self.options.qdf,
                        &static_map,
                        &self.removed_refs,
                    )?;
                } else {
                    root.write_object_qdf_with_ref_map_and_removed(
                        self.out,
                        0,
                        &static_map,
                        &self.removed_refs,
                    )?;
                }
            } else {
                let mut map = |child: &ObjectHandle| {
                    self.queue
                        .borrow_mut()
                        .enqueue_handle(self.pdf, child.clone())?
                        .ok_or_else(|| {
                            crate::Error::Unsupported(
                                "plain live writer: child is direct or removed".to_string(),
                            )
                        })
                };
                object.write_root_object_with_dynamic_ref_map(
                    self.out,
                    &mut map,
                    &self.removed_refs,
                    self.version,
                    self.final_extension_level,
                    true,
                )?;
            }
        } else if object.as_stream_dict().is_some() {
            let source = object.object_ref();
            let (dict, data, dictionary_options) = canonical_stream_output_for_rewrite(
                object,
                self.options,
                source.is_some_and(|source| {
                    self.options.content_normalization
                        && self.contents_sequences.contains_key(&source)
                }),
            )?;
            let surviving_children = crate::writer::object::prepared_stream_dictionary_children(
                &dict,
                dictionary_options,
            )?;
            self.enqueue_surviving_handles(surviving_children)?;
            let output_number = self.output_number(source.unwrap_or(ObjectRef::new(0, 0)))?;
            let emitted_ref = ObjectRef::new(output_number, 0);
            let encryption_context = self.encryption_context;
            let encrypt_stream = encryption_context
                .is_some_and(|context| context.encrypt_metadata || context.metadata_ref != source);
            let mut stream_length = data.len();
            if let Some(context) = encryption_context {
                crate::writer::adjust_aes_stream_length(
                    &mut stream_length,
                    context,
                    encrypt_stream,
                )?;
                dict.replace_key(
                    b"/Length",
                    ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                        crate::Error::Unsupported("stream /Length does not fit in i64".into())
                    })?),
                )?;
            }
            let queued_map = self.queue.borrow().old_to_new.clone();
            let static_map = |object_ref| map_queued_output(&queued_map, object_ref);
            if self.options.qdf {
                let holder = self
                    .queue
                    .borrow()
                    .stream_length_holders
                    .get(&source.unwrap_or(ObjectRef::new(output_number, 0)))
                    .copied()
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "plain live writer: stream {output_number} has no length holder"
                        ))
                    })?;
                if let Some(emitter) = self.encrypted_strings.as_mut() {
                    emitter.write_handle_stream_dict_with_ref_map(
                        self.out,
                        emitted_ref,
                        None,
                        &dict,
                        crate::writer::encrypted_strings::StreamDictOptions::new(
                            true,
                            dictionary_options,
                            encrypt_stream,
                        ),
                        &static_map,
                        &self.removed_refs,
                        Some(holder),
                    )?;
                } else {
                    dict.write_stream_body_qdf_with_ref_map_and_removed_and_length_with_options(
                        self.out,
                        0,
                        &static_map,
                        &self.removed_refs,
                        Some(holder),
                        dictionary_options,
                    )?;
                }
                if let Some(context) = encryption_context {
                    crate::writer::write_stream_payload_with_pipeline_qdf(
                        self.out,
                        &data,
                        self.options.newline_before_endstream,
                        true,
                        emitted_ref,
                        context,
                        encrypt_stream,
                        None,
                    )?;
                } else {
                    serialize::write_stream_payload_with_qdf(
                        self.out,
                        &data,
                        self.options.newline_before_endstream,
                        true,
                    )?;
                }
                self.current_stream_length = Some(IndirectStreamLength {
                    cur_stream_length: stream_length,
                    added_newline: serialize::framing_adds_newline_with_qdf(
                        &data,
                        self.options.newline_before_endstream,
                        true,
                    ),
                });
            } else {
                if let Some(emitter) = self.encrypted_strings.as_mut() {
                    emitter.write_handle_stream_dict_with_ref_map(
                        self.out,
                        emitted_ref,
                        None,
                        &dict,
                        crate::writer::encrypted_strings::StreamDictOptions::new(
                            false,
                            dictionary_options,
                            encrypt_stream,
                        ),
                        &static_map,
                        &self.removed_refs,
                        None,
                    )?;
                    crate::writer::write_stream_payload_with_pipeline(
                        self.out,
                        &data,
                        self.options.newline_before_endstream,
                        emitted_ref,
                        encryption_context.expect("encrypted emitter has a context"),
                        encrypt_stream,
                        None,
                    )?;
                } else {
                    dict.write_stream_body_with_ref_map_and_removed_with_options(
                        self.out,
                        dictionary_options,
                        &static_map,
                        &self.removed_refs,
                    )?;
                    serialize::write_stream_payload(
                        self.out,
                        &data,
                        self.options.newline_before_endstream,
                    )?;
                }
            }
        } else if self.encrypted_strings.is_some() || self.options.qdf {
            self.enqueue_surviving_children(object)?;
            let queued_map = self.queue.borrow().old_to_new.clone();
            let static_map = |object_ref| map_queued_output(&queued_map, object_ref);
            let output = self.output_number(object.object_ref().unwrap_or(ObjectRef::new(0, 0)))?;
            if let Some(emitter) = self.encrypted_strings.as_mut() {
                emitter.write_handle_object_with_ref_map(
                    self.out,
                    ObjectRef::new(output, 0),
                    None,
                    object,
                    self.options.qdf,
                    &static_map,
                    &self.removed_refs,
                )?;
            } else {
                object.write_object_qdf_with_ref_map_and_removed(
                    self.out,
                    0,
                    &static_map,
                    &self.removed_refs,
                )?;
            }
        } else {
            let mut map = |child: &ObjectHandle| {
                self.queue
                    .borrow_mut()
                    .enqueue_handle(self.pdf, child.clone())?
                    .ok_or_else(|| {
                        crate::Error::Unsupported(
                            "plain live writer: child is direct or removed".to_string(),
                        )
                    })
            };
            object.write_object_with_dynamic_ref_map(self.out, &mut map, &self.removed_refs)?;
        }
        Ok(())
    }
}

impl<'pdf, 'output, 'sink, R: Read + Seek + 'static> LiveObjectEmitter<'pdf, 'output, 'sink, R> {
    fn enqueue_surviving_handles(
        &mut self,
        children: impl IntoIterator<Item = ObjectHandle>,
    ) -> crate::Result<()> {
        for value in children {
            let mut indirect_children = Vec::new();
            collect_live_seed_handles(&value, &mut indirect_children, 0)?;
            for child in indirect_children {
                self.queue.borrow_mut().enqueue_handle(self.pdf, child)?;
            }
        }
        Ok(())
    }

    fn enqueue_surviving_children(&mut self, value: &ObjectHandle) -> crate::Result<()> {
        let mut children = Vec::new();
        collect_live_child_handles(value, &mut children, 0)?;
        self.enqueue_surviving_handles(children)
    }

    /// Emit a Preserve or Generate ObjStm from the live queue. QDF dispatches
    /// to the dedicated marker/pair-table framing below; this arm owns the
    /// compact (`libqpdf/QPDFWriter.cc:1665-1710` non-QDF) shape.
    /// References are resolved through the same dynamic, discovery-time map
    /// `unparse_object` uses, since a member's own children may not yet be
    /// queued when its body is serialized
    /// (`QPDFWriter::writeObjectStream`/`unparseChild`, `libqpdf/QPDFWriter.cc:1690-1697`).
    fn emit_live_object_stream(
        &mut self,
        container: &ObjectHandle,
        members: &[ObjectRef],
    ) -> crate::Result<()> {
        if self.options.qdf {
            return self.emit_live_qdf_object_stream(container, members);
        }
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
        let root_source = self.root_source;
        let removed_refs = self.removed_refs.clone();
        let body_writer = &mut |out: &mut Vec<u8>,
                                _member_index: u32,
                                _member_ref: ObjectRef,
                                handle: &ObjectHandle|
         -> crate::Result<()> {
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
            handle.try_dereference()?;
            let handle = if handle.as_stream_dict().is_some() {
                // qpdf's `writeObjectStream` warns through the member handle
                // and substitutes an indirect null before serialization
                // (`QPDFWriter.cc:1690-1705`). Preserve that writer-owned
                // damage policy instead of letting a stream payload enter an
                // ObjStm member body.
                // qpdf builds an ObjStm body twice (offset pass and write
                // pass), and this warning is inside that two-pass loop
                // (`QPDFWriter.cc:1621-1705`), so the same damaged member
                // warning is intentionally delivered twice.
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
            let result = crate::writer::output::with_buffer_sink(out, |out| {
                if handle.object_ref() == root_source {
                    handle.write_root_object_with_dynamic_ref_map(
                        out,
                        &mut map,
                        &removed_refs,
                        self.version,
                        self.final_extension_level,
                        true,
                    )
                } else {
                    handle.write_object_with_dynamic_ref_map(out, &mut map, &removed_refs)
                }
            });
            // cov:ignore-start: llvm-cov attributes the uncovered region to
            // this block's closing brace, the merge point for an exercised
            // member write failing (the sibling planned-writer copy of this
            // pattern, `emit_planned_object_stream`, has the same artifact).
            if result.is_ok() {
                crate::writer::report_progress_event(self.options)?;
            }
            // cov:ignore-end
            result
        };
        let body =
            object_streams::emit_objstm_body_from_handles_with_writer(&handles, body_writer)?;
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
        let offset = self.output_count()?;
        self.out
            .write_bytes(format!("{} {} obj\n", output.number, output.generation).as_bytes())?;
        if let Some(context) = self.encryption_context {
            serialize::write_encrypted_objstm_stream_with_extends(
                self.out,
                body,
                self.options.compress_streams,
                self.options.newline_before_endstream,
                extends,
                output,
                context,
            )?;
        } else {
            serialize::write_objstm_stream_with_extends(
                self.out,
                body,
                self.options.compress_streams,
                self.options.newline_before_endstream,
                extends,
            )?;
        } // cov:ignore: error arm requires an in-memory zlib encoder failure
        self.out.write_bytes(b"\nendobj\n")?;
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

    fn emit_live_qdf_object_stream(
        &mut self,
        container: &ObjectHandle,
        members: &[ObjectRef],
    ) -> crate::Result<()> {
        let container_source = container.object_ref().ok_or_else(|| {
            crate::Error::Internal(
                "plain live writer: QDF object-stream container has no source identity".into(),
            )
        })?;
        let output = self
            .queue
            .borrow()
            .old_to_new
            .get(&container_source)
            .copied()
            .ok_or_else(|| {
                crate::Error::Unsupported(format!(
                    "plain live writer: reference {} {} R absent from queue",
                    container_source.number, container_source.generation
                ))
            })?;
        let mut handles = Vec::with_capacity(members.len());
        for &member_source in members {
            let member_output = self
                .queue
                .borrow()
                .old_to_new
                .get(&member_source)
                .copied()
                .ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "plain live writer: object-stream member {} {} R absent from queue",
                        member_source.number, member_source.generation
                    ))
                })?;
            handles.push((member_output, self.pdf.get_object_handle(member_source)));
        }

        let root_source = self.root_source;
        let removed_refs = self.removed_refs.clone();
        let mut marker_starts = Vec::with_capacity(handles.len());
        let mut marker_lengths = Vec::with_capacity(handles.len());
        let body_writer = &mut |out: &mut Vec<u8>,
                                member_index: u32,
                                member_ref: ObjectRef,
                                handle: &ObjectHandle|
         -> crate::Result<()> {
            let marker_start = out.len();
            let source_member = usize::try_from(member_index)
                .ok()
                .and_then(|index| members.get(index))
                .copied()
                .unwrap_or(member_ref);
            out.extend_from_slice(
                format!(
                    "%% Object stream: object {}, index {}",
                    member_ref.number, member_index
                )
                .as_bytes(),
            );
            if !self.options.no_original_object_ids {
                let original = self.pdf.writer_original_object_ref(source_member);
                out.extend_from_slice(
                    format!("; original object ID: {}", original.number).as_bytes(),
                );
                if original.generation != 0 {
                    out.extend_from_slice(format!(" {}", original.generation).as_bytes());
                }
            }
            out.push(b'\n');
            marker_starts.push(marker_start);
            marker_lengths.push(out.len() - marker_start);
            if let Some(sequence) = self.page_sequences.get(&source_member) {
                out.extend_from_slice(format!("%% Page {sequence}\n").as_bytes());
            }

            handle.try_dereference()?;
            let handle_to_write = if handle.as_stream_dict().is_some() {
                for _ in 0..2 {
                    handle
                        .warn_if_possible("stream found inside object stream; treating as null")?;
                }
                ObjectHandle::null()
            } else {
                handle.clone()
            };
            self.enqueue_surviving_children(&handle_to_write)?;
            let result = if handle_to_write.object_ref() == root_source {
                let root = handle_to_write.output_root_copy_with_adbe(
                    self.version,
                    self.final_extension_level,
                    true,
                )?;
                self.enqueue_surviving_children(&root)?;
                let queued_map = self.queue.borrow().old_to_new.clone();
                let static_map = |object_ref| map_queued_output(&queued_map, object_ref);
                crate::writer::output::with_buffer_sink(out, |out| {
                    root.write_object_qdf_with_ref_map_and_removed(
                        out,
                        0,
                        &static_map,
                        &removed_refs,
                    )
                })
            } else {
                let queued_map = self.queue.borrow().old_to_new.clone();
                let static_map = |object_ref| map_queued_output(&queued_map, object_ref);
                crate::writer::output::with_buffer_sink(out, |out| {
                    handle_to_write.write_object_qdf_with_ref_map_and_removed(
                        out,
                        0,
                        &static_map,
                        &removed_refs,
                    )
                })
            };
            if result.is_ok() {
                crate::writer::report_progress_event(self.options)?;
            }
            result
        };
        let body =
            object_streams::emit_objstm_body_from_handles_with_writer_qdf(&handles, body_writer)?;
        let first_marker_len = marker_lengths.first().copied().ok_or_else(|| {
            crate::Error::Internal("plain live QDF ObjStm marker lengths are empty".into())
        })?;
        let mut body_bytes = body.bytes;
        body_bytes.drain(..body.first_offset);
        let mut pair_table = Vec::new();
        for (index, ((member, _), (&marker_start, &marker_len))) in handles
            .iter()
            .zip(marker_starts.iter().zip(marker_lengths.iter()))
            .enumerate()
        {
            if index != 0 {
                pair_table.push(b'\n');
            }
            let offset = marker_start
                .checked_add(marker_len)
                .and_then(|end| end.checked_sub(first_marker_len))
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "plain live QDF ObjStm member offset overflows usize".into(),
                    )
                })?;
            use std::io::Write as _;
            let _ = write!(pair_table, "{} {}", member.number, offset);
        }
        pair_table.push(b'\n');
        let first_offset = pair_table.len();
        let objects_len = body_bytes.len();
        body_bytes.reserve(first_offset);
        body_bytes.resize(
            objects_len.checked_add(first_offset).ok_or_else(|| {
                crate::Error::Unsupported(
                    "plain live QDF ObjStm body length overflows usize".into(),
                )
            })?,
            0,
        );
        body_bytes.copy_within(0..objects_len, first_offset);
        body_bytes[..first_offset].copy_from_slice(&pair_table);
        let qdf_first_offset = first_offset.checked_add(first_marker_len).ok_or_else(|| {
            crate::Error::Unsupported("plain live QDF ObjStm /First overflows usize".into())
        })?;
        let body = object_streams::ObjStmBody {
            bytes: body_bytes,
            first_offset,
            n_members: handles.len(),
        };

        let extends = {
            let source_handle = self.pdf.get_object_handle(container_source);
            source_handle.try_dereference()?;
            match source_handle.as_stream_dict() {
                Some(dict) => match dict.try_get_key(b"/Extends")?.object_ref() {
                    Some(extends) => {
                        let extends_handle = self.pdf.get_object_handle(extends);
                        Some(
                            self.queue
                                .borrow_mut()
                                .enqueue_handle(self.pdf, extends_handle)?
                                .ok_or_else(|| {
                                    crate::Error::Unsupported(
                                        "plain live writer: QDF object-stream /Extends is direct or removed"
                                            .into(),
                                    )
                                })?,
                        )
                    }
                    None => None,
                },
                None => None,
            }
        };
        let offset = self.output_count()?;
        self.out
            .write_bytes(format!("{} {} obj\n", output.number, output.generation).as_bytes())?;
        if let Some(context) = self.encryption_context {
            serialize::write_encrypted_objstm_stream_with_extends_qdf(
                self.out,
                body,
                extends,
                qdf_first_offset,
                self.options.newline_before_endstream,
                output,
                context,
            )?;
        } else {
            serialize::write_objstm_stream_with_extends_qdf(
                self.out,
                body,
                extends,
                qdf_first_offset,
                self.options.newline_before_endstream,
            )?;
        }
        self.out.write_bytes(b"\nendobj\n\n")?;
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
/// policy that belongs to encrypted output. PCLm calls this same writer-owned
/// policy after forcing qpdf's uncompressed setup; callers own only the final
/// stream framing around the returned one-stream buffer. The writer must never
/// append scan framing a second time.
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
    out: &mut OutputSink<'_>,
    map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> crate::Result<()> {
    let mut write_string =
        |out: &mut OutputSink<'_>, value: &[u8]| crate::pdf_syntax::write_string_value(out, value);
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
    out: &mut OutputSink<'_>,
    map: &dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    write_string: &mut F,
) -> crate::Result<()>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> crate::Result<()>,
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

struct ContentEmitter<'out, 'sink, 'map, 'removed, 'writer, F>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> crate::Result<()>,
{
    qdf: bool,
    out: &'out mut OutputSink<'sink>,
    map: &'map dyn Fn(ObjectRef) -> crate::Result<ObjectRef>,
    removed_refs: &'removed BTreeSet<ObjectRef>,
    write_string: &'writer mut F,
}

impl<F> ContentEmitter<'_, '_, '_, '_, '_, F>
where
    F: FnMut(&mut OutputSink<'_>, &[u8]) -> crate::Result<()>,
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
                            self.out.write_bytes(b"null")?;
                        } else {
                            self.out
                                .write_bytes((self.map)(object_ref)?.to_string().as_bytes())?;
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
            self.out.write_bytes(b"[\n")?;
            for item in items {
                push_spaces(self.out, indent + 2)?;
                self.emit_value(item, false, indent + 2)?;
                self.out.write_bytes(b"\n")?;
            }
            push_spaces(self.out, indent)?;
            self.out.write_bytes(b"]")?;
        } else {
            self.out.write_bytes(b"[")?;
            for item in items {
                self.out.write_bytes(b" ")?;
                self.emit_value(item, false, indent)?;
            }
            self.out.write_bytes(b" ]")?;
        }
        Ok(())
    }

    fn emit_dictionary(
        &mut self,
        entries: &BTreeMap<Vec<u8>, ObjectHandle>,
        indent: usize,
    ) -> crate::Result<()> {
        if self.qdf {
            self.out.write_bytes(b"<<\n")?;
        } else {
            self.out.write_bytes(b"<<")?;
        }

        for (key, value) in entries {
            if value.try_is_null()? || is_removed_content_reference(value, self.removed_refs) {
                continue;
            }
            if self.qdf {
                push_spaces(self.out, indent + 2)?;
            } else {
                self.out.write_bytes(b" ")?;
            }
            write_content_key(self.out, key)?;
            self.out.write_bytes(b" ")?;
            self.emit_value(value, false, indent + 2)?; // cov:ignore: LLVM does not attribute the successful nested emitter continuation
            if self.qdf {
                self.out.write_bytes(b"\n")?;
            }
        }

        if self.qdf {
            push_spaces(self.out, indent)?;
            self.out.write_bytes(b">>")?;
        } else {
            self.out.write_bytes(b" >>")?;
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
        self.out.write_bytes(b"\nstream\n")?;
        let payload = stream.get_raw_stream_data()?;
        let payload_result = self.out.write_bytes(payload.as_ref());
        let finish_result = self.out.finish_segment();
        payload_result?;
        finish_result?;
        self.out.write_bytes(b"\nendstream")
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

fn write_content_key(out: &mut OutputSink<'_>, key: &[u8]) -> crate::Result<()> {
    out.write_bytes(b"/")?;
    let key = key.strip_prefix(b"/").unwrap_or(key);
    crate::pdf_syntax::write_name_escaped(out, key)?;
    Ok(())
}

fn push_spaces(out: &mut OutputSink<'_>, count: usize) -> crate::Result<()> {
    const SPACES: &[u8; 64] = b"                                                                ";
    let mut remaining = count;
    while remaining > 0 {
        let chunk = remaining.min(SPACES.len());
        out.write_bytes(&SPACES[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
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

/// Probe whether the linearized writer will replace a stream's source filter
/// parameters under its qpdf writer policy.
///
/// `QPDFWriter::willFilterStream` is called with the state of the writer that
/// will emit the stream. The linearized planning caller must therefore pass
/// the same metadata and content-normalization policy as its emission route.
pub(crate) fn canonical_stream_will_be_refiltered_with_policy(
    handle: &ObjectHandle,
    options: &WriterOptions,
    apply_full_rewrite_metadata_policy: bool,
    normalize_content: bool,
) -> crate::Result<bool> {
    // Token filters are stateful qpdf ValueSetter-style consumers. Modified
    // streams are handled by the explicitly linearized-only probe below.
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
/// state. The linearized route must preserve that ownership and timing.
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
    let Some((encode_flags, decode_level, _normalized_content, _outer_filter)) =
        canonical_stream_filter_plan(
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
        if let Some((encode_flags, decode_level, normalized_content, outer_filter)) = filter_plan {
            let mut attempt = 1_u8;
            let (data, filtering_attempted, _source_success) = loop {
                let mut buffer =
                    crate::pipeline::buffer::Buffer::new("canonical writer stream", None);
                let mut actual_filtering_attempted = false;
                let (attempt_encode_flags, attempt_decode_level) = if attempt == 1 {
                    (encode_flags, decode_level)
                } else {
                    (0, crate::writer::DecodeLevel::None)
                };
                let source_success = source_for_pipe
                    .pipe_stream_data(
                        &mut buffer,
                        &mut actual_filtering_attempted,
                        attempt_encode_flags,
                        attempt_decode_level,
                        false,
                        attempt == 1,
                    )
                    .map_err(|error| stream_data_error(&source_for_pipe, error))?; // cov:ignore: filter-pipeline failures are covered at the pipeline boundary, not by this validated emitter

                if outer_filter && !actual_filtering_attempted && attempt == 1 {
                    // qpdf tests the separate `filtered` result returned by
                    // QPDFObjectHandle::pipeStreamData, not its source
                    // success result. A successful source/provider call that
                    // did not pass through an actual filter stage therefore
                    // receives exactly one fresh raw retry
                    // (`QPDFWriter.cc:1287-1314`).
                    attempt = 2;
                    continue;
                }
                // The source/provider success result is intentionally kept
                // separate from the filtering result above. Once the outer
                // filter has been disabled or the second attempt is reached,
                // qpdf keeps this attempt's buffer regardless of that source
                // result; terminal provider/pipeline errors have already
                // escaped as errors.
                break (
                    buffer.take_buffer()?,
                    actual_filtering_attempted,
                    source_success,
                );
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
    let dictionary_options = if filtering_attempted {
        StreamDictionaryOptions::new(
            true,
            matches!(policy, Some(CompressStreams::Yes)) && !normalized_content,
        )
    } else {
        StreamDictionaryOptions::preserve()
    };
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
    Ok(buffer.take_buffer()?)
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
) -> crate::Result<Option<(u32, crate::writer::DecodeLevel, bool, bool)>> {
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
    // Keep qpdf's outer `filter` request separate from the effective stream
    // policy used to choose the source pipe. In particular, `Some(No)` means
    // "use the canonical pipe without compression" but does not itself mean
    // that a filter stage was requested (`QPDFWriter.cc:1239-1284`).
    let outer_filter = handle.is_data_modified()
        || matches!(options.compress_streams, CompressStreams::Yes)
        || options.decode_level != crate::writer::DecodeLevel::None
        || is_metadata_stream
        || normalize_content;
    Ok(Some((
        encode_flags,
        decode_level,
        normalized_content,
        outer_filter,
    )))
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

#[cfg(test)]
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
            // qpdf applies the signature-dictionary exclusion only while
            // computing the ordinary compressible eligibility set. Its
            // `preserve-unreferenced` source-membership route deliberately
            // skips that filter (`QPDFWriter.cc:1939-1967`) and emits the
            // retained dictionary through `writeObjectStream`
            // (`QPDFWriter.cc:1621-1758`). Validate only body shapes that the
            // ObjStm format itself cannot represent here; reapplying the
            // signature predicate would reject a qpdf-valid preserved member.
            let violation = planned_member_body_violation(
                member.source,
                member.output,
                &member_handle,
                &context,
            )?; // cov:ignore: trailing `)?` on a multi-line validation call — llvm-cov attributes the validated continuation to the Err path
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

#[cfg(test)]
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

        crate::writer::output::with_buffer_sink(&mut output, |out| {
            emit_content_container_from_handle_with_ref_map(
                &container,
                &options,
                out,
                &map,
                &BTreeSet::new(),
            )
        })
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

    #[test]
    fn prepared_stream_children_drop_decode_parms_with_scalar_crypt() {
        let dictionary = ObjectHandle::dictionary(vec![
            (b"/Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec())),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/Name".to_vec(),
                    ObjectHandle::name(b"Identity".to_vec()),
                )]),
            ),
            (b"/Keep".to_vec(), ObjectHandle::integer(42)),
        ]);

        let surviving = crate::writer::object::prepared_stream_dictionary_children(
            &dictionary,
            StreamDictionaryOptions::preserve(),
        )
        .expect("prepare surviving stream children");
        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].unparse(), b"42");
    }

    #[test]
    fn live_queue_checks_owner_deduplicates_and_walks_direct_seed_containers() -> crate::Result<()>
    {
        let mut local_pdf = super::object_emitter_tests::pdf();
        let mut foreign_pdf = super::object_emitter_tests::pdf();
        let foreign = foreign_pdf
            .make_indirect_object_handle(ObjectHandle::integer(1))
            .unwrap();
        let mut queue = LiveQueue::new(BTreeSet::new(), false);
        let error = queue
            .enqueue_handle(&mut local_pdf, foreign)
            .expect_err("foreign live handles must be rejected");
        assert!(error.to_string().contains("different QPDF"));

        let child = local_pdf
            .make_indirect_object_handle(ObjectHandle::integer(42))
            .unwrap();
        let child_ref = child.object_ref().unwrap();
        let mut removed_queue = LiveQueue::new([child_ref].into_iter().collect(), false);
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
        let options = WriterOptions::default();
        let mut bytes = Vec::new();
        crate::writer::output::with_buffer_sink(&mut bytes, |out| {
            emit_live(
                &mut local_pdf,
                out,
                &options,
                "1.4",
                0,
                root_source,
                BTreeSet::new(),
                &[],
                None,
                &BTreeSet::new(),
            )
        })?; // cov:ignore: LLVM attributes the live-body test call terminator to callback cleanup.
        assert!(!bytes.is_empty());
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
        let mut queue = LiveQueue::new(BTreeSet::new(), false);
        queue.register_object_streams(
            &pdf,
            &[object_streams::ObjectStreamGroup::SourceBacked {
                source: container,
                members: vec![container],
            }],
        )?;

        let handle = pdf.get_object_handle(container);
        assert_eq!(queue.enqueue_handle(&mut pdf, handle)?, None);
        assert!(queue.old_to_new.is_empty());

        // A two-container cycle takes the same path one level deeper.
        let other = ObjectRef::new(6, 0);
        let mut queue = LiveQueue::new(BTreeSet::new(), false);
        queue.register_object_streams(
            &pdf,
            &[
                object_streams::ObjectStreamGroup::SourceBacked {
                    source: container,
                    members: vec![other],
                },
                object_streams::ObjectStreamGroup::SourceBacked {
                    source: other,
                    members: vec![container],
                },
            ],
        )?;
        let handle = pdf.get_object_handle(container);
        assert_eq!(queue.enqueue_handle(&mut pdf, handle)?, None);
        assert!(queue.old_to_new.is_empty());
        Ok(())
    }

    #[test]
    fn register_object_streams_skips_a_group_whose_members_are_all_removed() -> crate::Result<()> {
        let pdf = super::object_emitter_tests::pdf();
        let source = ObjectRef::new(3, 0);
        let member = ObjectRef::new(2, 0);
        let mut queue = LiveQueue::new([member].into_iter().collect(), false);
        queue.register_object_streams(
            &pdf,
            &[object_streams::ObjectStreamGroup::SourceBacked {
                source,
                members: vec![member],
            }],
        )?;
        assert!(queue.member_to_container.is_empty());
        assert!(queue.container_to_members.is_empty());
        Ok(())
    }

    #[test]
    fn removed_source_container_still_numbers_retained_members() -> crate::Result<()> {
        let container = ObjectRef::new(3, 0);
        let member = ObjectRef::new(2, 0);
        let mut pdf = super::object_emitter_tests::pdf();
        let mut queue = LiveQueue::new([container].into_iter().collect(), false);
        queue.register_object_streams(
            &pdf,
            &[object_streams::ObjectStreamGroup::SourceBacked {
                source: container,
                members: vec![member],
            }],
        )?;

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
    fn generated_members_reserve_numbers_when_the_container_is_enqueued() -> crate::Result<()> {
        let mut pdf = super::object_emitter_tests::pdf();
        let first = pdf.make_indirect_object_handle(ObjectHandle::integer(1))?;
        let second = pdf.make_indirect_object_handle(ObjectHandle::integer(2))?;
        let first_ref = first.object_ref().unwrap();
        let second_ref = second.object_ref().unwrap();
        let container = pdf.make_indirect_object_handle(ObjectHandle::null())?;
        let container_ref = container.object_ref().unwrap();
        let mut queue = LiveQueue::new(BTreeSet::new(), false);
        queue.register_object_streams(
            &pdf,
            &[object_streams::ObjectStreamGroup::Generated {
                source: container_ref,
                members: vec![second_ref, first_ref],
            }],
        )?;

        assert_eq!(
            queue.enqueue_handle(&mut pdf, first)?,
            Some(ObjectRef::new(2, 0))
        );
        assert_eq!(
            queue.old_to_new.get(&container_ref),
            Some(&ObjectRef::new(1, 0))
        );
        assert_eq!(
            queue.old_to_new.get(&first_ref),
            Some(&ObjectRef::new(2, 0))
        );
        assert_eq!(
            queue.old_to_new.get(&second_ref),
            Some(&ObjectRef::new(3, 0))
        );
        Ok(())
    }

    #[test]
    fn qdf_queue_reserves_a_length_holder_after_each_stream() -> crate::Result<()> {
        let mut pdf = super::object_emitter_tests::pdf();
        let old_xref = pdf.new_stream_with_data(Rc::new(b"old xref".to_vec()))?;
        old_xref
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Type", ObjectHandle::name(b"XRef".to_vec()))?;
        let stream = pdf.new_stream_with_data(Rc::new(b"stream".to_vec()))?;
        let stream_ref = stream.object_ref().unwrap();
        let scalar = pdf.make_indirect_object_handle(ObjectHandle::integer(42))?;
        let scalar_ref = scalar.object_ref().unwrap();
        let mut queue = LiveQueue::new(BTreeSet::new(), true);

        assert_eq!(queue.enqueue_handle(&mut pdf, old_xref)?, None);
        assert_eq!(
            queue.enqueue_handle(&mut pdf, stream)?,
            Some(ObjectRef::new(1, 0))
        );
        assert_eq!(
            queue.stream_length_holders.get(&stream_ref),
            Some(&ObjectRef::new(2, 0))
        );
        assert_eq!(
            queue.enqueue_handle(&mut pdf, scalar)?,
            Some(ObjectRef::new(3, 0))
        );
        assert_eq!(
            queue.old_to_new.get(&scalar_ref),
            Some(&ObjectRef::new(3, 0))
        );
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
        let options = WriterOptions::default();
        let mut bytes = Vec::new();
        crate::writer::output::with_buffer_sink(&mut bytes, |out| {
            emit_live(
                &mut pdf,
                out,
                &options,
                "1.5",
                0,
                root_source,
                BTreeSet::new(),
                &object_streams,
                None,
                &BTreeSet::new(),
            )
        })?; // cov:ignore: LLVM attributes the live-body test call terminator to callback cleanup.
        assert!(bytes
            .windows(b"/Type /ObjStm".len())
            .any(|window| window == b"/Type /ObjStm"));
        assert!(!bytes
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
        let options = WriterOptions::default();
        let mut bytes = Vec::new();
        let body = crate::writer::output::with_buffer_sink(&mut bytes, |out| {
            emit_live(
                &mut pdf,
                out,
                &options,
                "1.5",
                0,
                root_source,
                BTreeSet::new(),
                &object_streams,
                None,
                &BTreeSet::new(),
            )
        })?; // cov:ignore: LLVM attributes the live-body test call terminator to callback cleanup.
        let predecessor_output = body
            .old_to_new
            .get(&predecessor_id)
            .expect("an unreferenced /Extends predecessor is still discovered and numbered");
        let expected = format!("/Extends {} 0 R", predecessor_output.number);
        assert!(bytes
            .windows(expected.len())
            .any(|window| window == expected.as_bytes()));
        Ok(())
    }
}
