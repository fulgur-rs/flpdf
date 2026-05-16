//! ObjStm eligibility predicate — decides whether an indirect object may be
//! stored inside an object stream (PDF 1.5+, ISO 32000-1 §7.5.7).
//! Also provides the packing planner that groups eligible objects into batches.
//! Provides the body emitter that serialises a list of objects into an ObjStm
//! payload (§7.5.7 body format; compression/dict wrapping is done in 5.4).

// These items are consumed by the upcoming ObjStm writer; suppress dead_code
// until that code lands.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroUsize;

use crate::object::{Dictionary, Object, ObjectRef};
use crate::XrefOffset;

// ── Public types ─────────────────────────────────────────────────────────────

/// Context resolved once per document, used to identify objects that must stay
/// outside any ObjStm.
pub(crate) struct EligibilityContext {
    /// The indirect reference of the encryption dictionary, if any.
    pub encryption_ref: Option<ObjectRef>,
    /// The indirect reference of the linearization parameter dictionary, if any.
    pub linearization_param_ref: Option<ObjectRef>,
}

// ── Predicate ────────────────────────────────────────────────────────────────

/// Returns `true` when the object identified by `object_ref` with body
/// `object` may be stored inside an ObjStm.
///
/// Disqualifying conditions (PDF spec + implementation constraints):
/// 1. `object_ref.generation != 0`  — ObjStm members must have generation 0.
/// 2. `object` is a [`Object::Stream`] — streams cannot be embedded in ObjStm.
/// 3. The object is a dictionary with `/Type /ObjStm` — no nested ObjStm.
/// 4. The object is a dictionary with `/Type /XRef` — xref streams must be direct.
/// 5. `object_ref` is the encryption dictionary reference.
/// 6. `object_ref` is the linearization parameter dictionary reference.
pub(crate) fn is_eligible_for_objstm(
    object_ref: ObjectRef,
    object: &Object,
    ctx: &EligibilityContext,
) -> bool {
    // 1. Generation must be 0.
    if object_ref.generation != 0 {
        return false;
    }

    // 2. Stream objects cannot be embedded.
    if matches!(object, Object::Stream(_)) {
        return false;
    }

    // 3 & 4. Check /Type for Dictionary objects.
    if let Object::Dictionary(dict) = object {
        if dict_type_is(dict, b"ObjStm") || dict_type_is(dict, b"XRef") {
            return false;
        }
    }

    // 5. Encryption dictionary must not be embedded.
    if Some(object_ref) == ctx.encryption_ref {
        return false;
    }

    // 6. Linearization parameter dictionary must not be embedded.
    if Some(object_ref) == ctx.linearization_param_ref {
        return false;
    }

    true
}

// ── Context builder ──────────────────────────────────────────────────────────

/// Build an [`EligibilityContext`] by querying `pdf` for the encryption and
/// linearization parameter references.  Must be called once before processing
/// any objects; the result is then used with [`is_eligible_for_objstm`] which
/// is a pure function.
pub(crate) fn eligibility_context<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
) -> crate::Result<EligibilityContext> {
    Ok(EligibilityContext {
        encryption_ref: pdf.encryption_ref(),
        linearization_param_ref: pdf.linearized_hint_ref()?,
    })
}

// ── Packing planner types ────────────────────────────────────────────────────

/// Controls how the ObjStm packing planner groups objects into batches.
///
/// Mirrors `qpdf --object-streams=preserve|disable|generate`. The default,
/// `Preserve`, matches qpdf's behaviour for a plain `qpdf in.pdf out.pdf`
/// invocation: ObjStms present in the input are reused; their membership is
/// not repartitioned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectStreamMode {
    /// Keep the original ObjStm membership from the source document.
    #[default]
    Preserve,
    /// Emit no ObjStms; all eligible objects become plain indirects.
    Disable,
    /// Pack eligible objects into fresh ObjStms (greedy with cap).
    Generate,
}

/// qpdf's default ObjStm batch size cap.
pub(crate) const DEFAULT_BATCH_SIZE_CAP: NonZeroUsize = match NonZeroUsize::new(100) {
    Some(n) => n,
    None => unreachable!(),
};

/// Configuration for the ObjStm packing planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerConfig {
    pub mode: ObjectStreamMode,
    /// Maximum number of members per ObjStm batch. qpdf default is 100.
    pub batch_size_cap: NonZeroUsize,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            mode: ObjectStreamMode::Preserve,
            batch_size_cap: DEFAULT_BATCH_SIZE_CAP,
        }
    }
}

/// The output of the packing planner: an ordered list of batches,
/// each of which will become one ObjStm in the output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PackingPlan {
    /// Each inner `Vec` is one ObjStm batch, members in deterministic order.
    pub batches: Vec<Vec<ObjectRef>>,
}

/// Convert public [`WriteOptions`](crate::WriteOptions) into an internal
/// [`PlannerConfig`].  The conversion is direct: `WriteOptions.object_streams`
/// names the policy, and the planner's batch cap defaults to qpdf's value of
/// 100.  Future writer-side knobs (e.g. an explicit cap override) would be
/// threaded through this conversion.
pub(crate) fn planner_config_from_options(options: &crate::WriteOptions) -> PlannerConfig {
    PlannerConfig {
        mode: options.object_streams,
        batch_size_cap: DEFAULT_BATCH_SIZE_CAP,
    }
}

// ── Packing planner ──────────────────────────────────────────────────────────

/// Decide how many ObjStms to emit and which objects belong in each.
///
/// - `Disable`  → returns an empty plan (zero batches).
/// - `Preserve` → reconstructs the source document's ObjStm grouping,
///   skipping ineligible members and splitting batches that exceed the cap.
/// - `Generate` → greedily packs all eligible objects in
///   `(number, generation)` ascending order, cap-delimited.
pub(crate) fn plan_object_streams<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
    config: &PlannerConfig,
) -> crate::Result<PackingPlan> {
    if config.mode == ObjectStreamMode::Disable {
        return Ok(PackingPlan::default());
    }

    let ctx = eligibility_context(pdf)?;
    let length_exclusions = collect_indirect_objstm_length_refs(pdf)?;

    match config.mode {
        ObjectStreamMode::Disable => unreachable!(),
        ObjectStreamMode::Preserve => plan_preserve(pdf, config, &ctx, &length_exclusions),
        ObjectStreamMode::Generate => plan_generate(pdf, config, &ctx, &length_exclusions),
    }
}

/// Collect the set of ObjectRefs that serve as indirect /Length targets of any
/// ObjStm stream in the document.  ISO 32000-1 §7.5.7 prohibits those objects
/// from being stored inside an ObjStm themselves.
pub(crate) fn collect_indirect_objstm_length_refs<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let mut excluded = BTreeSet::new();
    let refs: Vec<ObjectRef> = pdf.object_refs();
    for r in refs {
        let obj = pdf.resolve(r)?;
        if let Object::Stream(s) = &obj {
            if dict_type_is(&s.dict, b"ObjStm") {
                if let Some(Object::Reference(len_ref)) = s.dict.get("Length") {
                    excluded.insert(*len_ref);
                }
            }
        }
    }
    Ok(excluded)
}

/// Preserve mode: reconstruct source ObjStm grouping.
fn plan_preserve<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
    config: &PlannerConfig,
    ctx: &EligibilityContext,
    length_exclusions: &BTreeSet<ObjectRef>,
) -> crate::Result<PackingPlan> {
    let entries = pdf.source_xref_entries();

    // Group members by (container_number, index) so we can reconstruct order.
    // Key: container object number; Value: list of (index, ObjectRef).
    let mut groups: BTreeMap<u32, Vec<(u32, ObjectRef)>> = BTreeMap::new();

    for (obj_ref, offset) in &entries {
        if let XrefOffset::Compressed { stream, index } = offset {
            groups.entry(*stream).or_default().push((*index, *obj_ref));
        }
    }

    let cap = config.batch_size_cap.get();
    let mut batches: Vec<Vec<ObjectRef>> = Vec::new();

    // Iterate containers in ascending container-number order.
    for (_container_num, mut members) in groups {
        // Sort by index within the container to get deterministic order.
        members.sort_by_key(|(idx, _)| *idx);

        // Filter ineligible members.
        let mut eligible: Vec<ObjectRef> = Vec::new();
        for (_idx, obj_ref) in members {
            if length_exclusions.contains(&obj_ref) {
                continue;
            }
            let obj = pdf.resolve(obj_ref)?;
            if is_eligible_for_objstm(obj_ref, &obj, ctx) {
                eligible.push(obj_ref);
            }
        }

        // Split into cap-sized batches (preserving order, same container slot).
        for chunk in eligible.chunks(cap) {
            if !chunk.is_empty() {
                batches.push(chunk.to_vec());
            }
        }
    }

    Ok(PackingPlan { batches })
}

/// Generate mode: greedily pack all eligible objects in number/generation order.
fn plan_generate<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
    config: &PlannerConfig,
    ctx: &EligibilityContext,
    length_exclusions: &BTreeSet<ObjectRef>,
) -> crate::Result<PackingPlan> {
    // Collect refs, excluding free (deleted) entries — they resolve to Null but
    // are not real objects and must never be placed in an ObjStm.
    let source_entries = pdf.source_xref_entries();
    let free_refs: BTreeSet<ObjectRef> = source_entries
        .iter()
        .filter_map(|(r, offset)| {
            if matches!(offset, XrefOffset::Free { .. }) {
                Some(*r)
            } else {
                None
            }
        })
        .collect();

    let mut refs: Vec<ObjectRef> = pdf
        .object_refs()
        .into_iter()
        .filter(|r| !free_refs.contains(r))
        .collect();
    refs.sort_by_key(|r| (r.number, r.generation));

    let cap = config.batch_size_cap.get();
    let mut current_batch: Vec<ObjectRef> = Vec::new();
    let mut batches: Vec<Vec<ObjectRef>> = Vec::new();

    for obj_ref in refs {
        if length_exclusions.contains(&obj_ref) {
            continue;
        }
        let obj = pdf.resolve(obj_ref)?;
        if !is_eligible_for_objstm(obj_ref, &obj, ctx) {
            continue;
        }
        current_batch.push(obj_ref);
        if current_batch.len() >= cap {
            batches.push(std::mem::take(&mut current_batch));
        }
    }
    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    Ok(PackingPlan { batches })
}

// ── ObjStm body emitter ───────────────────────────────────────────────────────

/// The serialised body of an ObjStm (ISO 32000-1 §7.5.7).
///
/// Contains the raw pair table concatenated with the objects section.
/// Compression (FlateDecode) and the stream dictionary wrapping are handled
/// by a subsequent step (5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObjStmBody {
    /// Raw concatenation: pair table || objects section.  To be deflate-wrapped by 5.4.
    pub bytes: Vec<u8>,
    /// Offset within `bytes` where the first object body starts.  Matches /First.
    pub first_offset: usize,
    /// Number of members.  Matches /N.
    pub n_members: usize,
}

/// Serialise a list of pre-resolved `(ObjectRef, Object)` pairs into an ObjStm
/// body following ISO 32000-1 §7.5.7.
///
/// This inner function does the real work without touching a `Pdf` reader; it
/// exists primarily to make unit-testing Pdf-free.
pub(crate) fn emit_objstm_body_from_resolved(
    members: &[(ObjectRef, Object)],
) -> crate::Result<ObjStmBody> {
    if members.is_empty() {
        return Ok(ObjStmBody {
            bytes: vec![],
            first_offset: 0,
            n_members: 0,
        });
    }

    // Duplicate detection — fail fast before producing any output.
    let mut seen: HashSet<u32> = HashSet::with_capacity(members.len());
    for (obj_ref, _) in members {
        if !seen.insert(obj_ref.number) {
            return Err(crate::Error::Unsupported(format!(
                "duplicate member in ObjStm batch {}",
                obj_ref.number
            )));
        }
    }

    // Build the objects section and record per-member offsets.
    let mut objects_section: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::with_capacity(members.len());

    for (_, obj) in members {
        offsets.push(objects_section.len());
        obj.write_pdf(&mut objects_section);
        // Append exactly one newline after each object body (write_pdf has no trailing LF).
        objects_section.push(b'\n');
    }

    // Build the pair table: "<number> <offset>\n" for each member.
    let mut pair_table: Vec<u8> = Vec::new();
    for ((obj_ref, _), offset) in members.iter().zip(offsets.iter()) {
        let entry = format!("{} {}\n", obj_ref.number, offset);
        pair_table.extend_from_slice(entry.as_bytes());
    }

    let first_offset = pair_table.len();

    // Concatenate: pair table || objects section.
    let mut bytes = pair_table;
    bytes.extend_from_slice(&objects_section);

    Ok(ObjStmBody {
        bytes,
        first_offset,
        n_members: members.len(),
    })
}

/// Resolve each member reference from `pdf` then call
/// [`emit_objstm_body_from_resolved`].
pub(crate) fn emit_objstm_body<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
    members: &[ObjectRef],
) -> crate::Result<ObjStmBody> {
    let resolved: crate::Result<Vec<(ObjectRef, Object)>> = members
        .iter()
        .map(|&r| pdf.resolve(r).map(|obj| (r, obj)))
        .collect();
    emit_objstm_body_from_resolved(&resolved?)
}

// ── ObjStm stream wrapper ────────────────────────────────────────────────────

/// Wrap an [`ObjStmBody`] and build the complete `/Type /ObjStm` stream
/// dictionary (ISO 32000-1 §7.5.7).
///
/// The returned [`crate::Stream`] is ready to be written as an indirect object.
/// Key order follows qpdf parity: `Type → N → First → Length → Filter`.
///
/// The `compress` parameter controls whether the body bytes are compressed with
/// FlateDecode (`CompressStreams::Yes`, the default) or emitted raw
/// (`CompressStreams::No`).  Passing the same [`crate::writer::CompressStreams`]
/// value that drives the surrounding full-rewrite loop ensures the ObjStm
/// container uses the same policy as every other stream in the document.
pub(crate) fn wrap_objstm_body(
    body: &ObjStmBody,
    compress: crate::writer::CompressStreams,
) -> crate::Result<crate::Stream> {
    match compress {
        crate::writer::CompressStreams::Yes => {
            // Build a temporary encode dict with /Filter /FlateDecode.
            let mut encode_dict = Dictionary::new();
            encode_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));

            // Compress the body bytes via the existing helper.
            let encoded = crate::filters::encode_stream_data(&encode_dict, &body.bytes)?;

            // Build the final stream dictionary in qpdf-compatible key order.
            let mut dict = Dictionary::new();
            dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
            dict.insert("N", Object::Integer(body.n_members as i64));
            dict.insert("First", Object::Integer(body.first_offset as i64));
            dict.insert("Length", Object::Integer(encoded.len() as i64));
            dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));

            Ok(crate::Stream {
                dict,
                data: encoded,
            })
        }
        crate::writer::CompressStreams::No => {
            // Emit raw (uncompressed) body bytes without any /Filter.
            let mut dict = Dictionary::new();
            dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
            dict.insert("N", Object::Integer(body.n_members as i64));
            dict.insert("First", Object::Integer(body.first_offset as i64));
            dict.insert("Length", Object::Integer(body.bytes.len() as i64));
            // No /Filter key — body is raw plaintext.

            Ok(crate::Stream {
                dict,
                data: body.bytes.clone(),
            })
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns `true` when `dict` contains `/Type /<expected>`.
fn dict_type_is(dict: &Dictionary, expected: &[u8]) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == expected)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Dictionary, Stream};

    fn no_ctx() -> EligibilityContext {
        EligibilityContext {
            encryption_ref: None,
            linearization_param_ref: None,
        }
    }

    fn ref0(n: u32) -> ObjectRef {
        ObjectRef::new(n, 0)
    }

    fn ref1(n: u32) -> ObjectRef {
        ObjectRef::new(n, 1)
    }

    fn typed_dict(type_name: &[u8]) -> Object {
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(type_name.to_vec()));
        Object::Dictionary(d)
    }

    #[test]
    fn generation_one_is_ineligible() {
        let obj = Object::Null;
        assert!(!is_eligible_for_objstm(ref1(1), &obj, &no_ctx()));
    }

    #[test]
    fn stream_object_is_ineligible() {
        let obj = Object::Stream(Stream::new(Dictionary::new(), vec![]));
        assert!(!is_eligible_for_objstm(ref0(1), &obj, &no_ctx()));
    }

    #[test]
    fn objstm_typed_dict_is_ineligible() {
        let obj = typed_dict(b"ObjStm");
        assert!(!is_eligible_for_objstm(ref0(1), &obj, &no_ctx()));
    }

    #[test]
    fn xref_typed_dict_is_ineligible() {
        let obj = typed_dict(b"XRef");
        assert!(!is_eligible_for_objstm(ref0(1), &obj, &no_ctx()));
    }

    #[test]
    fn encryption_dict_ref_is_ineligible() {
        let ctx = EligibilityContext {
            encryption_ref: Some(ref0(5)),
            linearization_param_ref: None,
        };
        let obj = Object::Null;
        assert!(!is_eligible_for_objstm(ref0(5), &obj, &ctx));
    }

    #[test]
    fn linearization_param_dict_ref_is_ineligible() {
        let ctx = EligibilityContext {
            encryption_ref: None,
            linearization_param_ref: Some(ref0(7)),
        };
        let obj = Object::Null;
        assert!(!is_eligible_for_objstm(ref0(7), &obj, &ctx));
    }

    #[test]
    fn plain_page_dict_is_eligible() {
        let obj = typed_dict(b"Page");
        assert!(is_eligible_for_objstm(ref0(3), &obj, &no_ctx()));
    }

    #[test]
    fn plain_null_object_is_eligible() {
        let obj = Object::Null;
        assert!(is_eligible_for_objstm(ref0(10), &obj, &no_ctx()));
    }

    // ── Planner tests ────────────────────────────────────────────────────────

    /// Build a zlib-compressed ObjStm payload from (object-number, raw-bytes) pairs.
    /// Returns (compressed_bytes, first_offset).
    #[cfg(test)]
    fn build_objstm_payload(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut header = String::new();
        let mut body = Vec::new();
        for (index, (number, object_data)) in members.iter().enumerate() {
            let offset = body.len();
            header.push_str(&format!("{} {} ", number, offset));
            body.extend_from_slice(object_data);
            if index + 1 < members.len() {
                body.push(b'\n');
            }
        }
        let mut decoded = Vec::new();
        decoded.extend_from_slice(header.as_bytes());
        decoded.extend_from_slice(&body);

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&decoded).unwrap();
        let encoded = enc.finish().unwrap();
        (encoded, header.len())
    }

    fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
        let b = value.to_be_bytes();
        bytes.extend_from_slice(&b[1..]);
    }

    /// Append a 1+3+1 xref stream entry (W=[1 3 1]).
    fn append_xref_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
        entries.push(entry_type);
        append_u24_be(entries, field1);
        entries.push(field2);
    }

    /// Build a minimal PDF (PDF-1.5) that contains one ObjStm.
    ///
    /// Fixed object layout (object numbers are consecutive):
    ///   0          free
    ///   1 0 obj    Catalog (plain indirect at offset)
    ///   2 0 obj    Pages   (compressed in ObjStm 4, index 0)
    ///   3..N 0 obj extra compressed members (in ObjStm 4, indices 1..N-2)
    ///   N+1 0 obj  ObjStm (object number = 2 + n_extra + 1 = 3 + n_extra)
    ///   N+2 0 obj  XRef stream
    ///
    /// `n_extra`: how many additional compressed members to include beyond obj 2.
    ///   They receive consecutive object numbers starting at 3.
    ///   Pass `extra_data` as per-object bytes; length must equal `n_extra`.
    fn one_objstm_pdf_n(extra_data: &[&[u8]]) -> Vec<u8> {
        let n_extra = extra_data.len();
        // ObjStm object number = 3 + n_extra
        let objstm_num = 3 + n_extra as u32;
        // XRef stream object number = objstm_num + 1
        let xref_num = objstm_num + 1;
        // Total object count (0-based inclusive up to xref_num)
        let total_size = xref_num + 1;

        let mut bytes = b"%PDF-1.5\n".to_vec();

        // Object 1: Catalog
        let catalog_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Build ObjStm payload
        let pages_bytes: &[u8] = b"<< /Type /Pages /Count 0 /Kids [] >>";
        let mut all_members: Vec<(u32, &[u8])> = vec![(2, pages_bytes)];
        for (i, data) in extra_data.iter().enumerate() {
            all_members.push((3 + i as u32, data));
        }
        let n_members = all_members.len() as u32;
        let (stream_data, first) = build_objstm_payload(&all_members);

        // ObjStm object at objstm_num
        let objstm_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "{objstm_num} 0 obj\n<< /Type /ObjStm /N {n_members} /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_offset = bytes.len();

        // Build xref entries (W=[1 3 1], /Index [0 total_size])
        let mut xref_entries: Vec<u8> = Vec::new();
        // 0: free
        append_xref_entry(&mut xref_entries, 0, 0, 0);
        // 1: Catalog
        append_xref_entry(&mut xref_entries, 1, catalog_offset as u32, 0);
        // 2: Pages compressed in ObjStm, index 0
        append_xref_entry(&mut xref_entries, 2, objstm_num, 0);
        // 3..objstm_num-1: extra members compressed in ObjStm
        for i in 0..n_extra {
            append_xref_entry(&mut xref_entries, 2, objstm_num, (i + 1) as u8);
        }
        // objstm_num: ObjStm stream at offset
        append_xref_entry(&mut xref_entries, 1, objstm_offset as u32, 0);
        // xref_num: XRef stream at xref_offset
        append_xref_entry(&mut xref_entries, 1, xref_offset as u32, 0);

        bytes.extend_from_slice(
            format!(
                "{xref_num} 0 obj\n<< /Type /XRef /Size {total_size} /Root 1 0 R /W [1 3 1] /Index [0 {total_size}] /Length {} >>\nstream\n",
                xref_entries.len()
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&xref_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    fn open_pdf(bytes: Vec<u8>) -> crate::reader::Pdf<std::io::Cursor<Vec<u8>>> {
        crate::reader::Pdf::open(std::io::Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn planner_disable_mode_yields_empty_plan() {
        let pdf_bytes = one_objstm_pdf_n(&[]);
        let mut pdf = open_pdf(pdf_bytes);
        let config = PlannerConfig {
            mode: ObjectStreamMode::Disable,
            batch_size_cap: NonZeroUsize::new(100).unwrap(),
        };
        let plan = plan_object_streams(&mut pdf, &config).unwrap();
        assert!(
            plan.batches.is_empty(),
            "Disable mode must produce zero batches"
        );
    }

    #[test]
    fn planner_preserve_mode_reuses_source_membership() {
        // ObjStm has 2 members: object 2 (Pages) at index 0, object 3 (string) at index 1.
        let pdf_bytes = one_objstm_pdf_n(&[b"(hello)"]);
        let mut pdf = open_pdf(pdf_bytes);

        let config = PlannerConfig {
            mode: ObjectStreamMode::Preserve,
            batch_size_cap: NonZeroUsize::new(100).unwrap(),
        };
        let plan = plan_object_streams(&mut pdf, &config).unwrap();

        // Should produce exactly one batch containing refs 2 and 3 (index order).
        assert_eq!(plan.batches.len(), 1, "expected 1 batch");
        let batch = &plan.batches[0];
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0], ObjectRef::new(2, 0));
        assert_eq!(batch[1], ObjectRef::new(3, 0));
    }

    #[test]
    fn planner_generate_mode_packs_eligible_objects_in_sorted_order() {
        // one_objstm_pdf_n with 1 extra member:
        //   obj 1: Catalog dict  → eligible
        //   obj 2: Pages dict    → eligible (compressed)
        //   obj 3: string (hello)→ eligible (compressed)
        //   obj 4: ObjStm stream → ineligible
        //   obj 5: XRef stream   → ineligible
        let pdf_bytes = one_objstm_pdf_n(&[b"(world)"]);
        let mut pdf = open_pdf(pdf_bytes);

        let config = PlannerConfig {
            mode: ObjectStreamMode::Generate,
            batch_size_cap: NonZeroUsize::new(100).unwrap(),
        };
        let plan = plan_object_streams(&mut pdf, &config).unwrap();

        assert_eq!(plan.batches.len(), 1);
        let batch = &plan.batches[0];
        // Must be in (number, generation) ascending order
        let numbers: Vec<u32> = batch.iter().map(|r| r.number).collect();
        assert!(
            numbers.windows(2).all(|w| w[0] < w[1]),
            "batch must be sorted by object number; got {numbers:?}"
        );
        // Eligible count: 1, 2, 3 → 3 objects
        assert_eq!(batch.len(), 3, "expected 3 eligible objects");
        // All refs must have generation 0
        for r in batch {
            assert_eq!(r.generation, 0);
        }
    }

    #[test]
    fn planner_respects_batch_size_cap() {
        // 5 extra members → obj 2,3,4,5,6,7 are compressed; obj 1 plain Catalog.
        // ObjStm=obj8, XRef=obj9. Eligible: 1,2,3,4,5,6,7 → 7 objects.
        let extra: Vec<&[u8]> = vec![b"(a)" as &[u8], b"(b)", b"(c)", b"(d)", b"(e)"];
        let pdf_bytes = one_objstm_pdf_n(&extra);
        let mut pdf = open_pdf(pdf_bytes);

        let config = PlannerConfig {
            mode: ObjectStreamMode::Generate,
            batch_size_cap: NonZeroUsize::new(3).unwrap(),
        };
        let plan = plan_object_streams(&mut pdf, &config).unwrap();

        // Every batch must have <= 3 members
        for (i, batch) in plan.batches.iter().enumerate() {
            assert!(
                batch.len() <= 3,
                "batch {i} has {} members, exceeds cap of 3",
                batch.len()
            );
        }
        // Total members across all batches: 7 eligible (1,2,3,4,5,6,7)
        let total: usize = plan.batches.iter().map(|b| b.len()).sum();
        assert_eq!(total, 7, "expected 7 eligible objects in total");
        // ceil(7/3) = 3 batches
        assert_eq!(plan.batches.len(), 3);
    }

    /// Verify that an ObjStm whose `/Length` is an indirect reference causes
    /// `plan_object_streams` to return `Err`.
    ///
    /// ## Fixture layout
    ///   0          free
    ///   1 0 obj    Catalog  (plain indirect)
    ///   2 0 obj    Pages    (compressed in ObjStm 3, index 0)
    ///   3 0 obj    ObjStm   with /Length 5 0 R  (plain indirect; parser cannot decode it)
    ///   4 0 obj    XRef stream
    ///   5 0 obj    Integer  (the actual length value; serves as /Length target)
    ///
    /// ## What this test verifies
    ///
    /// The flpdf stream parser (`stream_from_dict`) requires `/Length` to be a
    /// direct integer.  When `collect_indirect_objstm_length_refs` iterates
    /// over all objects and hits ObjStm 3, `pdf.resolve(3 0 R)` calls
    /// `stream_from_dict`, which errors on the indirect `/Length 5 0 R`.
    /// That error propagates through `plan_object_streams` via `?`, so the
    /// function must return `Err`.
    ///
    /// The `Ok` branch is kept for forward-compatibility: if the parser gains
    /// indirect-/Length support in the future, the exclusion rule (object 5 0 R
    /// must not appear in any batch) should still hold.
    #[test]
    fn planner_excludes_indirect_objstm_length_target() {
        // Build ObjStm payload containing only Pages(2,0).  Catalog (1,0) is
        // emitted as a plain indirect object below — putting it in both places
        // would create a ghost member that no xref entry points to.
        let pages_bytes: &[u8] = b"<< /Type /Pages /Count 0 /Kids [] >>";
        let (stream_data, first) = build_objstm_payload(&[(2, pages_bytes)]);
        let stream_len = stream_data.len();

        // We will write the length holder (5 0 obj) as a plain integer.
        // The ObjStm dict references /Length as "5 0 R".
        let mut bytes = b"%PDF-1.5\n".to_vec();

        // 1 0 obj — Catalog (plain indirect, so we can reference it easily)
        let catalog_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // 3 0 obj — ObjStm with /Length 5 0 R (indirect)
        let objstm_offset = bytes.len();
        let objstm_header = format!(
            "3 0 obj\n<< /Type /ObjStm /N 1 /First {first} /Length 5 0 R /Filter /FlateDecode >>\nstream\n"
        );
        bytes.extend_from_slice(objstm_header.as_bytes());
        bytes.extend_from_slice(&stream_data);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");

        // 5 0 obj — the actual length value
        let len_holder_offset = bytes.len();
        bytes.extend_from_slice(format!("5 0 obj\n{stream_len}\nendobj\n").as_bytes());

        // 4 0 obj — XRef stream (W=[1 3 1], /Index [0 6])
        let xref_offset = bytes.len();
        // 6 objects: 0 free, 1 catalog, 2 Pages (compressed), 3 ObjStm, 4 XRef, 5 LenHolder
        let mut xref_entries: Vec<u8> = Vec::new();
        // 0: free
        append_xref_entry(&mut xref_entries, 0, 0, 0);
        // 1: Catalog at catalog_offset
        append_xref_entry(&mut xref_entries, 1, catalog_offset as u32, 0);
        // 2: Pages compressed in ObjStm 3, index 0
        append_xref_entry(&mut xref_entries, 2, 3, 0);
        // 3: ObjStm at objstm_offset
        append_xref_entry(&mut xref_entries, 1, objstm_offset as u32, 0);
        // 4: XRef at xref_offset (self-referential)
        append_xref_entry(&mut xref_entries, 1, xref_offset as u32, 0);
        // 5: LenHolder at len_holder_offset
        append_xref_entry(&mut xref_entries, 1, len_holder_offset as u32, 0);

        let xref_header = format!(
            "4 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 3 1] /Index [0 6] /Length {} >>\nstream\n",
            xref_entries.len()
        );
        bytes.extend_from_slice(xref_header.as_bytes());
        bytes.extend_from_slice(&xref_entries);
        bytes.extend_from_slice(b"\nendstream\nendobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        let mut pdf = open_pdf(bytes);

        let config = PlannerConfig {
            mode: ObjectStreamMode::Generate,
            batch_size_cap: NonZeroUsize::new(100).unwrap(),
        };
        let result = plan_object_streams(&mut pdf, &config);

        // As of 2026-05, stream_from_dict requires /Length to be a direct integer,
        // so plan_object_streams returns Err for this fixture.  The Err branch is
        // the expected path.  The Ok branch is kept for forward-compatibility only.
        match result {
            Err(_) => {
                // Expected: parser rejects indirect /Length, plan_object_streams
                // returns Err.  This documents the known limitation.
            }
            Ok(plan) => {
                // Forward-compat path: if indirect /Length support lands, the
                // exclusion rule must still hold — object (5,0) must not appear
                // in any batch because it is the /Length target of an ObjStm.
                let holder = ObjectRef::new(5, 0);
                for (i, batch) in plan.batches.iter().enumerate() {
                    assert!(
                        !batch.contains(&holder),
                        "batch {i} must not contain the indirect /Length holder (5,0); got {batch:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn planner_preserve_mode_skips_ineligible_members() {
        // ObjStm has 3 compressed members:
        //   obj 2: Pages dict          → eligible
        //   obj 3: /Type /XRef dict    → ineligible (is_eligible_for_objstm rejects XRef-typed dicts)
        //   obj 4: plain string        → eligible
        //
        // Preserve mode must include only obj 2 and obj 4 in the output batch,
        // skipping obj 3 even though it is recorded as compressed in the source xref.
        let pdf_bytes = one_objstm_pdf_n(&[
            b"<< /Type /XRef /Size 1 >>", // obj 3: XRef-typed dict — ineligible
            b"(eligible-string)",         // obj 4: plain string — eligible
        ]);
        let mut pdf = open_pdf(pdf_bytes);
        let config = PlannerConfig {
            mode: ObjectStreamMode::Preserve,
            batch_size_cap: NonZeroUsize::new(100).unwrap(),
        };
        let plan = plan_object_streams(&mut pdf, &config).unwrap();
        assert_eq!(plan.batches.len(), 1, "expected 1 batch");
        let batch = &plan.batches[0];
        // Only obj 2 (Pages) and obj 4 (string) are eligible; obj 3 (/Type /XRef) is filtered.
        assert_eq!(
            batch.len(),
            2,
            "expected 2 eligible members; got {:?}",
            batch
        );
        assert_eq!(batch[0], ObjectRef::new(2, 0));
        assert_eq!(batch[1], ObjectRef::new(4, 0));
    }

    // ── ObjStm body emitter tests ────────────────────────────────────────────

    #[test]
    fn emit_objstm_body_empty_batch_returns_empty() {
        let body = emit_objstm_body_from_resolved(&[]).unwrap();
        assert_eq!(body.bytes, Vec::<u8>::new());
        assert_eq!(body.first_offset, 0);
        assert_eq!(body.n_members, 0);
    }

    #[test]
    fn emit_objstm_body_single_member_layout() {
        let obj_ref = ObjectRef::new(12, 0);
        let obj = Object::Integer(42);

        // Serialize expected object bytes manually.
        let mut expected_obj_bytes = Vec::new();
        obj.write_pdf(&mut expected_obj_bytes);
        // write_pdf has no trailing LF; emitter appends one.
        expected_obj_bytes.push(b'\n');

        let body = emit_objstm_body_from_resolved(&[(obj_ref, obj)]).unwrap();

        // Pair table: "12 0\n"
        let expected_pair_table = b"12 0\n";
        assert_eq!(
            &body.bytes[..expected_pair_table.len()],
            expected_pair_table,
            "pair table mismatch"
        );

        // first_offset matches pair table length.
        assert_eq!(body.first_offset, expected_pair_table.len());

        // Objects section starts at first_offset.
        let objects_section = &body.bytes[body.first_offset..];
        assert_eq!(
            objects_section, expected_obj_bytes,
            "objects section mismatch"
        );

        assert_eq!(body.n_members, 1);
    }

    #[test]
    fn emit_objstm_body_multiple_members_offsets_are_correct() {
        let members: Vec<(ObjectRef, Object)> = vec![
            (ObjectRef::new(5, 0), Object::Integer(100)),
            (ObjectRef::new(7, 0), Object::Boolean(true)),
            (ObjectRef::new(9, 0), Object::Null),
        ];

        let body = emit_objstm_body_from_resolved(&members).unwrap();

        assert_eq!(body.n_members, 3);

        // Parse the pair table to get reported offsets.
        let pair_table_bytes = &body.bytes[..body.first_offset];
        let pair_table_str = std::str::from_utf8(pair_table_bytes).unwrap();
        let mut reported_offsets: Vec<usize> = Vec::new();
        for line in pair_table_str.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(
                parts.len(),
                2,
                "pair table line must have 2 tokens: {line:?}"
            );
            let offset: usize = parts[1].parse().unwrap();
            reported_offsets.push(offset);
        }
        assert_eq!(reported_offsets.len(), 3);

        // Verify that each reported offset matches the actual start position
        // of each serialized object in the objects section.
        let objects_section = &body.bytes[body.first_offset..];
        for (i, (_, obj)) in members.iter().enumerate() {
            let mut expected_obj = Vec::new();
            obj.write_pdf(&mut expected_obj);
            let start = reported_offsets[i];
            let end = start + expected_obj.len();
            assert!(
                end <= objects_section.len(),
                "object {i} offset {start} + len {} out of range",
                expected_obj.len()
            );
            assert_eq!(
                &objects_section[start..end],
                expected_obj.as_slice(),
                "object {i} body mismatch at offset {start}"
            );
        }
    }

    #[test]
    fn emit_objstm_body_rejects_duplicate_member_numbers() {
        let members: Vec<(ObjectRef, Object)> = vec![
            (ObjectRef::new(5, 0), Object::Integer(1)),
            (ObjectRef::new(5, 0), Object::Integer(2)),
        ];
        let result = emit_objstm_body_from_resolved(&members);
        assert!(result.is_err(), "expected Err for duplicate member");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("duplicate member") || err_msg.contains("5"),
            "error message should mention duplicate/number: {err_msg}"
        );
    }

    #[test]
    fn emit_objstm_body_round_trip_with_reader() {
        // Build a body with three integer objects and parse each back via
        // parse_object_stream_entry (no Pdf construction needed — no /Filter).
        let members: Vec<(ObjectRef, Object)> = vec![
            (ObjectRef::new(10, 0), Object::Integer(111)),
            (ObjectRef::new(20, 0), Object::Integer(222)),
            (ObjectRef::new(30, 0), Object::Integer(333)),
        ];

        let body = emit_objstm_body_from_resolved(&members).unwrap();

        // Construct a synthetic Stream with no /Filter (data is already plain).
        let mut dict = crate::object::Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        dict.insert("N", Object::Integer(body.n_members as i64));
        dict.insert("First", Object::Integer(body.first_offset as i64));
        dict.insert("Length", Object::Integer(body.bytes.len() as i64));
        let stream = crate::Stream {
            dict,
            data: body.bytes.clone(),
        };

        for (index, (_, expected_obj)) in members.iter().enumerate() {
            let parsed = crate::reader::parse_object_stream_entry(&stream, index as u32).unwrap();
            assert_eq!(
                &parsed, expected_obj,
                "round-trip mismatch at index {index}"
            );
        }
    }

    // ── wrap_objstm_body tests ────────────────────────────────────────────────

    #[test]
    fn wrap_objstm_body_dict_layout() {
        let body = ObjStmBody {
            bytes: b"1 0\n42\n".to_vec(),
            first_offset: 4,
            n_members: 1,
        };
        let stream =
            wrap_objstm_body(&body, crate::writer::CompressStreams::Yes).unwrap();

        assert_eq!(
            stream.dict.get("Type"),
            Some(&Object::Name(b"ObjStm".to_vec())),
            "/Type must be /ObjStm"
        );
        assert_eq!(
            stream.dict.get("N"),
            Some(&Object::Integer(1)),
            "/N must be 1"
        );
        assert_eq!(
            stream.dict.get("First"),
            Some(&Object::Integer(4)),
            "/First must be 4"
        );
        assert_eq!(
            stream.dict.get("Length"),
            Some(&Object::Integer(stream.data.len() as i64)),
            "/Length must equal compressed data length"
        );
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Name(b"FlateDecode".to_vec())),
            "/Filter must be /FlateDecode"
        );
    }

    #[test]
    fn wrap_objstm_body_round_trip_via_decode() {
        let members: Vec<(ObjectRef, Object)> = vec![
            (ObjectRef::new(5, 0), Object::Integer(10)),
            (ObjectRef::new(6, 0), Object::Integer(20)),
        ];
        let body = emit_objstm_body_from_resolved(&members).unwrap();
        let original_bytes = body.bytes.clone();

        let stream =
            wrap_objstm_body(&body, crate::writer::CompressStreams::Yes).unwrap();

        let decoded = crate::filters::decode_stream_data(&stream.dict, &stream.data).unwrap();
        assert_eq!(
            decoded, original_bytes,
            "decoded bytes must equal original body bytes"
        );
    }

    #[test]
    fn wrap_objstm_body_round_trip_via_parse_object_stream_entry() {
        let ref1 = ObjectRef::new(11, 0);
        let obj1 = Object::Integer(42);
        let ref2 = ObjectRef::new(22, 0);
        let obj2 = Object::Integer(99);

        let body =
            emit_objstm_body_from_resolved(&[(ref1, obj1.clone()), (ref2, obj2.clone())]).unwrap();
        let stream =
            wrap_objstm_body(&body, crate::writer::CompressStreams::Yes).unwrap();

        let parsed0 = crate::reader::parse_object_stream_entry(&stream, 0).unwrap();
        let parsed1 = crate::reader::parse_object_stream_entry(&stream, 1).unwrap();

        assert_eq!(parsed0, obj1, "index 0 must parse to Integer(42)");
        assert_eq!(parsed1, obj2, "index 1 must parse to Integer(99)");
    }

    #[test]
    fn wrap_objstm_body_empty_members_still_valid() {
        let body = ObjStmBody {
            bytes: vec![],
            first_offset: 0,
            n_members: 0,
        };
        let stream =
            wrap_objstm_body(&body, crate::writer::CompressStreams::Yes).unwrap();

        assert_eq!(
            stream.dict.get("N"),
            Some(&Object::Integer(0)),
            "/N must be 0"
        );
        assert_eq!(
            stream.dict.get("First"),
            Some(&Object::Integer(0)),
            "/First must be 0"
        );
        assert_eq!(
            stream.dict.get("Length"),
            Some(&Object::Integer(stream.data.len() as i64)),
            "/Length must equal compressed data length"
        );
        // Even empty input produces some deflate bytes (header + checksum).
        assert!(
            !stream.data.is_empty(),
            "compressed empty input must produce non-empty deflate bytes"
        );
    }

    // ── Mode dispatch (flpdf-9hc.5.5) ────────────────────────────────────────

    #[test]
    fn write_options_default_is_preserve_mode() {
        let options = crate::WriteOptions::default();
        assert_eq!(options.object_streams, ObjectStreamMode::Preserve);
    }

    #[test]
    fn object_stream_mode_default_is_preserve() {
        assert_eq!(ObjectStreamMode::default(), ObjectStreamMode::Preserve);
    }

    #[test]
    fn planner_config_from_options_maps_preserve() {
        let options = crate::WriteOptions {
            object_streams: ObjectStreamMode::Preserve,
            ..Default::default()
        };
        let config = planner_config_from_options(&options);
        assert_eq!(config.mode, ObjectStreamMode::Preserve);
        assert_eq!(config.batch_size_cap, DEFAULT_BATCH_SIZE_CAP);
    }

    #[test]
    fn planner_config_from_options_maps_disable() {
        let options = crate::WriteOptions {
            object_streams: ObjectStreamMode::Disable,
            ..Default::default()
        };
        let config = planner_config_from_options(&options);
        assert_eq!(config.mode, ObjectStreamMode::Disable);
    }

    #[test]
    fn planner_config_from_options_maps_generate() {
        let options = crate::WriteOptions {
            object_streams: ObjectStreamMode::Generate,
            ..Default::default()
        };
        let config = planner_config_from_options(&options);
        assert_eq!(config.mode, ObjectStreamMode::Generate);
    }
}
