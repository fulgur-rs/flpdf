//! qpdf correspondence: QPDF_optimization.cc inherited-page-attribute push.
//!
//! Deviation: null checks on an inheritable key's value use the canonical
//! handle resolver before inspecting the value. This keeps lazy indirect
//! values aligned with qpdf's accessors without introducing a second value
//! snapshot route. See `pages.rs`'s
//! `resolve_inherited_handle_with_max_depth` for the corresponding
//! compensation in the sibling bottom-up attribute climb, and the inline
//! deviation-marker comments below for the exact call sites.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object_handle::is_scalar;
use crate::pages::repair::{PageTreeRoot, PreparedPages};
use crate::{Error, Pdf, Result};
use crate::{ObjectHandle, ObjectRef};

const INHERITABLE_KEYS: [&[u8]; 4] = [b"/CropBox", b"/MediaBox", b"/Resources", b"/Rotate"];

pub(crate) fn push<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    prepared: &PreparedPages,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    let mut key_ancestors: BTreeMap<&'static [u8], Vec<ObjectHandle>> = BTreeMap::new();
    let mut visited = BTreeSet::new();
    match prepared.root {
        PageTreeRoot::Indirect(root) => push_internal(
            pdf,
            root,
            &mut key_ancestors,
            &mut visited,
            allow_changes,
            warn_skipped_keys,
        )?,
        PageTreeRoot::Direct { catalog } => {
            let catalog = pdf.get_object_handle(catalog);
            push_direct_root(
                pdf,
                catalog,
                &mut key_ancestors,
                &mut visited,
                allow_changes,
                warn_skipped_keys,
            )? // cov:ignore: llvm-cov attributes this covered multi-line Direct branch terminator to the call setup
        }
        PageTreeRoot::DirectCatalog => {
            let catalog = pdf.root_handle()?;
            push_direct_root(
                pdf,
                catalog,
                &mut key_ancestors,
                &mut visited,
                allow_changes,
                warn_skipped_keys,
            )? // cov:ignore: llvm-cov attributes this covered multi-line DirectCatalog branch terminator to the call setup
        }
    }
    debug_assert!(
        key_ancestors.values().all(Vec::is_empty),
        "key_ancestors not empty after pushing inherited attributes to pages"
    );
    Ok(())
}

fn push_direct_root<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    catalog: ObjectHandle,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    // cov:ignore-start: PreparedPages::Direct is created and consumed without an intervening public mutation
    pdf.resolve(&catalog)?;
    let pages = catalog.get_key(b"/Pages");
    if pages.as_dictionary().is_none() {
        return Ok(());
    }
    // cov:ignore-end
    push_direct_node(
        pdf,
        &pages,
        key_ancestors,
        visited,
        allow_changes,
        warn_skipped_keys,
    )?;
    Ok(())
}

fn push_direct_node<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    // cov:ignore-start: LLVM maps this covered frame-construction call to an untracked terminator
    let Some(frame) = enter_direct_frame(
        pdf,
        dict.clone(),
        key_ancestors,
        allow_changes,
        warn_skipped_keys,
    )?
    // cov:ignore-end
    else {
        return Ok(());
    };
    walk_inherited_frames(
        pdf,
        vec![frame],
        key_ancestors,
        visited,
        allow_changes,
        warn_skipped_keys,
    )
}

fn push_node_attributes<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    allow_changes: bool,
) -> Result<Vec<&'static [u8]>> {
    let mut own_keys = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict
            .as_dictionary()
            .and_then(|entries| entries.get(key).cloned())
        else {
            continue;
        };
        // Resolve the live value before applying qpdf's null-as-absent rule.
        if pdf.resolve_handle(&value)?.is_null() {
            continue;
        }
        if !allow_changes {
            return Err(Error::Unsupported(
                "optimize detected an inheritable attribute when called in no-change mode"
                    .to_owned(),
            ));
        }
        dict.remove_key(key);
        let value = if value.is_indirect() {
            value
        } else if !is_scalar(&value)? {
            pdf.make_indirect_from_object_handle(value)?
        } else {
            value
        };
        key_ancestors.entry(key).or_default().push(value);
        own_keys.push(key);
    }
    Ok(own_keys)
}

fn pop_node_attributes(
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    own_keys: Vec<&'static [u8]>,
) {
    for key in own_keys {
        let stack = key_ancestors
            .get_mut(key)
            .expect("own inherited key must have an ancestor stack");
        stack.pop();
        if stack.is_empty() {
            key_ancestors.remove(key);
        }
    }
}

#[derive(Clone, Copy)]
enum InheritedFrameKind {
    Direct,
    Indirect,
}

struct InheritedFrame {
    kind: InheritedFrameKind,
    kids: Vec<ObjectHandle>,
    next_kid: usize,
    own_keys: Vec<&'static [u8]>,
}

fn warn_skipped_pages_keys<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    warn_skipped_keys: bool,
    object: String,
) -> Result<()> {
    // cov:ignore-start: all production callers pass warn_skipped_keys=false
    if warn_skipped_keys && dict.try_has_key(b"/Parent")? {
        for key in dict.try_get_keys()? {
            if !INHERITABLE_KEYS.contains(&key.as_slice())
                && ![b"/Type".as_slice(), b"/Parent", b"/Kids", b"/Count"].contains(&key.as_slice())
            {
                pdf.push_qpdf_warning(crate::QpdfExc::new(
                    crate::QpdfErrorCode::DamagedPdf,
                    pdf.input_description(),
                    object.clone(),
                    0,
                    format!(
                        "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
                        String::from_utf8_lossy(key.strip_prefix(b"/").unwrap_or(&key)),
                    ),
                ))?;
            }
        }
    }
    // cov:ignore-end
    Ok(())
}

fn enter_direct_frame<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: ObjectHandle,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<Option<InheritedFrame>> {
    if !is_pages_dictionary(&dict) {
        return Ok(None);
    }
    let object = dict
        .object_ref()
        .map(|r| format!("Pages object: object {} {}", r.number, r.generation))
        .unwrap_or_else(|| "Pages object".to_owned());
    warn_skipped_pages_keys(pdf, &dict, warn_skipped_keys, object)?;
    let own_keys = push_node_attributes(pdf, &dict, key_ancestors, allow_changes)?;
    let kids = dict.get_key(b"/Kids").as_array().unwrap_or_default();
    Ok(Some(InheritedFrame {
        kind: InheritedFrameKind::Direct,
        kids,
        next_kid: 0,
        own_keys,
    }))
}

fn enter_indirect_frame<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<Option<InheritedFrame>> {
    if !visited.insert(node_ref) {
        return Ok(None); // cov:ignore: page-tree repair rejects cycles before inherited-attribute push
    }
    let dict = pdf.get_object_handle(node_ref);
    pdf.resolve(&dict)?;
    if dict.as_dictionary().is_none() || !is_pages_dictionary(&dict) {
        return Ok(None);
    }
    warn_skipped_pages_keys(
        pdf,
        &dict,
        warn_skipped_keys,
        format!(
            "Pages object: object {} {}",
            node_ref.number, node_ref.generation
        ),
    )?;
    let own_keys = push_node_attributes(pdf, &dict, key_ancestors, allow_changes)?;
    let kids = dict.get_key(b"/Kids").as_array().unwrap_or_default();
    Ok(Some(InheritedFrame {
        kind: InheritedFrameKind::Indirect,
        kids,
        next_kid: 0,
        own_keys,
    }))
}

fn walk_inherited_frames<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    initial: Vec<InheritedFrame>,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    let mut frames = initial;
    while let Some(frame) = frames.last_mut() {
        if frame.next_kid >= frame.kids.len() {
            let finished = frames.pop().expect("frame exists");
            pop_node_attributes(key_ancestors, finished.own_keys);
            continue;
        }

        let kid = frame.kids[frame.next_kid].clone();
        frame.next_kid += 1;
        match frame.kind {
            InheritedFrameKind::Direct => {
                if let Some(kid_ref) = handle_reference(&kid) {
                    if let Some(child) = push_child_reference(
                        pdf,
                        kid_ref,
                        key_ancestors,
                        visited,
                        allow_changes,
                        warn_skipped_keys,
                    )? {
                        frames.push(child);
                    }
                } else if kid.as_dictionary().is_some() && kid.has_key(b"/Kids") {
                    if let Some(child) = enter_direct_frame(
                        pdf,
                        kid,
                        key_ancestors,
                        allow_changes,
                        warn_skipped_keys,
                    )? {
                        frames.push(child);
                    }
                } // cov:ignore: LLVM maps the covered direct-reference frame branch terminator separately
            }
            InheritedFrameKind::Indirect => {
                if let Some(kid_ref) = handle_reference(&kid) {
                    if let Some(child) = push_child_reference(
                        pdf,
                        kid_ref,
                        key_ancestors,
                        visited,
                        allow_changes,
                        warn_skipped_keys,
                    )? {
                        frames.push(child);
                    }
                } // cov:ignore: LLVM maps the covered indirect-reference frame branch terminator separately
            }
        }
    }
    Ok(())
}

fn push_child_reference<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    kid_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<Option<InheritedFrame>> {
    let child = pdf.get_object_handle(kid_ref);
    pdf.resolve(&child)?;
    if is_pages_dictionary(&child) {
        return enter_indirect_frame(
            pdf,
            kid_ref,
            key_ancestors,
            visited,
            allow_changes,
            warn_skipped_keys,
        );
    }

    if child.as_dictionary().is_none() {
        return Ok(None); // cov:ignore: page-tree repair guarantees indirect children are dictionaries
    }
    for (&key, values) in key_ancestors.iter() {
        let present = match child
            .as_dictionary()
            .and_then(|entries| entries.get(key).cloned())
        {
            None => false,
            // Resolve the live value before applying qpdf's null-as-absent rule.
            Some(value) => !pdf.resolve_handle(&value)?.is_null(),
        };
        if !present {
            if let Some(value) = values.last() {
                child.replace_key(key, value.clone())?;
            }
        }
    }
    Ok(None)
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<ObjectHandle>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    let Some(frame) = enter_indirect_frame(
        pdf,
        node_ref,
        key_ancestors,
        visited,
        allow_changes,
        warn_skipped_keys,
    )?
    else {
        return Ok(());
    };
    walk_inherited_frames(
        pdf,
        vec![frame],
        key_ancestors,
        visited,
        allow_changes,
        warn_skipped_keys,
    )
}

fn is_pages_dictionary(handle: &ObjectHandle) -> bool {
    handle
        .as_dictionary()
        .and_then(|entries| entries.get(b"/Type".as_slice()).cloned())
        .and_then(|value| value.as_name())
        .is_some_and(|name| name == b"Pages")
}

fn handle_reference(handle: &ObjectHandle) -> Option<ObjectRef> {
    handle.object_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::PipelineHandle;
    use crate::{ObjectHandle, Pdf};

    fn pdf_bytes(bodies: &[(u32, &[u8])]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0_u64; bodies.last().map_or(1, |(number, _)| *number as usize + 1)];
        for &(number, body) in bodies {
            offsets[number as usize] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len() as u64;
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                bodies.last().map_or(1, |(number, _)| number + 1)
            )
            .as_bytes(),
        );
        pdf
    }

    fn pdf_with_inherited_scalar_rotate() -> Vec<u8> {
        pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
        ])
    }

    fn pdf_with_unknown_intermediate_pages_key() -> Vec<u8> {
        pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (
                3,
                b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /Unknown 7 >>",
            ),
            (4, b"<< /Type /Page /Parent 3 0 R >>"),
        ])
    }

    #[test]
    fn no_change_mode_rejects_inheritable_key_before_mutation() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_inherited_scalar_rotate()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        let PageTreeRoot::Indirect(root) = prepared.root else {
            // cov:ignore-start: fixture catalog has an indirect /Pages root
            panic!("fixture has an indirect /Pages root");
            // cov:ignore-end
        };
        let root_handle = pdf.get_object_handle(root);
        pdf.resolve(&root_handle).unwrap();
        let before = root_handle.get_key(b"/Rotate").as_integer();

        let error = push(&mut pdf, &prepared, false, false).unwrap_err();

        assert!(error.to_string().contains("inheritable attribute"));
        assert_eq!(root_handle.get_key(b"/Rotate").as_integer(), before);
    }

    #[test]
    fn pushing_an_inherited_key_onto_a_leaf_preserves_its_source_extent() {
        // The fixture's page 3 lacks /Rotate, so it actually inherits
        // /Rotate 90 from its /Pages parent -- exercising
        // `push_child_reference`'s `changed` branch, not the unchanged-leaf
        // skip. qpdf's `kid.replaceKey` never clears the leaf's recorded
        // source extent for this same mutation; the leaf's extent here must
        // match.
        let mut pdf = Pdf::open_mem_owned(pdf_with_inherited_scalar_rotate()).unwrap();
        let leaf_ref = ObjectRef::new(3, 0);
        pdf.get_object_handle(leaf_ref).try_dereference().unwrap();
        let before = pdf.get_object_handle(leaf_ref).end_offsets();
        assert_ne!(
            before,
            (-1, -1),
            "fixture leaf must have a real parse extent"
        );

        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        push(&mut pdf, &prepared, true, false).unwrap();

        let leaf = pdf.get_object_handle(leaf_ref);
        pdf.resolve(&leaf).unwrap();
        assert_eq!(
            leaf.get_key(b"/Rotate").as_integer(),
            Some(90),
            "leaf must have actually inherited /Rotate"
        );
        assert_eq!(
            pdf.get_object_handle(leaf_ref).end_offsets(),
            before,
            "inheriting a key must not clear the leaf's source extent"
        );
    }

    #[test]
    fn direct_pages_root_walks_direct_descendant_and_reference_kids() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (
                1,
                b"<< /Type /Catalog /Pages << /Type /Pages /Kids [<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>] /Count 1 >> >>",
            ),
            (
                3,
                b"<< /Type /Page /MediaBox [0 0 612 792] >>",
            ),
        ]))
        .unwrap();
        let prepared = PreparedPages {
            root: PageTreeRoot::Direct {
                catalog: ObjectRef::new(1, 0),
            },
            pages: vec![ObjectRef::new(3, 0)],
        };

        push(&mut pdf, &prepared, true, false).expect("direct root walk");

        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        pdf.resolve(&page).expect("resolve direct-root page");
        assert_eq!(page.get_key(b"/Rotate").as_integer(), Some(90));
    }

    #[test]
    fn warning_mode_reports_unknown_intermediate_pages_key() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_unknown_intermediate_pages_key()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();

        push(&mut pdf, &prepared, true, true).unwrap();

        assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
            diagnostic.message_string().contains("Unknown key /Unknown")
                && diagnostic.message_string().contains("/Pages")
        }));
        let page = pdf.get_object_handle(prepared.pages[0]);
        pdf.resolve(&page).unwrap();
        assert!(page.has_key(b"/MediaBox"));
    }

    #[test]
    fn warning_sink_failure_propagates_from_an_intermediate_pages_key() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_unknown_intermediate_pages_key()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        let logger = crate::QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(NthWriteFailure::new(1))));
        pdf.set_logger(logger);

        assert!(matches!(
            push(&mut pdf, &prepared, true, true),
            Err(crate::Error::System(ref message)) if message == "sink write failure 1"
        ));
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|diagnostic| diagnostic.message_string().contains("Unknown key /Unknown")));
    }

    #[test]
    fn deep_page_tree_push_has_no_arbitrary_depth_cap() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [] /Count 0 >>"),
        ]))
        .unwrap();
        let depth = 120;
        for depth in 0..depth {
            let number = 2 + depth as u32;
            let child = number + 1;
            let node = ObjectHandle::dictionary(vec![
                (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
                (
                    b"/Kids".to_vec(),
                    ObjectHandle::array(vec![pdf.get_object_handle(ObjectRef::new(child, 0))]),
                ),
                (b"/Count".to_vec(), ObjectHandle::integer(0)),
            ]);
            pdf.replace_object(ObjectRef::new(number, 0), node).unwrap();
        }
        let boundary = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
            (b"/Kids".to_vec(), ObjectHandle::array(Vec::new())),
            (b"/Count".to_vec(), ObjectHandle::integer(0)),
        ]);
        pdf.replace_object(ObjectRef::new(2 + depth as u32, 0), boundary)
            .unwrap();
        let prepared = PreparedPages {
            root: PageTreeRoot::Indirect(ObjectRef::new(2, 0)),
            pages: Vec::new(),
        };

        push(&mut pdf, &prepared, true, false)
            .expect("qpdf's inherited-attribute push has no arbitrary depth cap");
    }

    /// `direct-root-adbe.pdf`'s trailer `/Root` is an inline Catalog dict
    /// whose `/Pages` is also inline, so `prepare_for_optimization` selects
    /// `PageTreeRoot::DirectCatalog` — the one `push()` dispatch arm that had
    /// no covering test (unlike `PageTreeRoot::Indirect` above and
    /// `PageTreeRoot::Direct`, exercised through `direct_root_writer_tests.rs`).
    #[test]
    fn direct_catalog_root_dispatches_through_push_direct_root() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/direct-root-adbe.pdf");
        let mut pdf = Pdf::open_mem_owned(std::fs::read(path).expect("fixture exists"))
            .expect("fixture opens");
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .expect("prepare_for_optimization")
            .expect("page tree resolves");
        assert!(
            matches!(prepared.root, PageTreeRoot::DirectCatalog),
            "fixture must select the DirectCatalog dispatch arm: {:?}",
            prepared.root
        );

        push(&mut pdf, &prepared, true, false).expect("push over a direct Catalog root");
    }

    #[test]
    fn explicit_frames_cover_direct_indirect_and_non_pages_entries() {
        let mut pdf = Pdf::open_mem_owned(pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>"),
            (4, b"<< /Type /Page /MediaBox [0 0 612 792] >>"),
        ]))
        .unwrap();
        let mut key_ancestors = BTreeMap::new();
        let mut visited = BTreeSet::new();
        push_internal(
            &mut pdf,
            ObjectRef::new(2, 0),
            &mut key_ancestors,
            &mut visited,
            true,
            false,
        )
        .expect("indirect child Pages frame");

        let direct_root = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
            (
                b"/Kids".to_vec(),
                ObjectHandle::array(vec![
                    pdf.get_object_handle(ObjectRef::new(3, 0)),
                    ObjectHandle::dictionary(vec![
                        (b"/Type".to_vec(), ObjectHandle::name(b"Pages".to_vec())),
                        (
                            b"/Kids".to_vec(),
                            ObjectHandle::array(vec![pdf.get_object_handle(ObjectRef::new(4, 0))]),
                        ),
                    ]),
                ]),
            ),
        ]);
        let mut key_ancestors = BTreeMap::new();
        let mut visited = BTreeSet::new();
        push_direct_node(
            &mut pdf,
            &direct_root,
            &mut key_ancestors,
            &mut visited,
            true,
            false,
        )
        .expect("direct child Pages frame");

        let scalar = ObjectHandle::integer(7);
        let mut key_ancestors = BTreeMap::new();
        let mut visited = BTreeSet::new();
        push_direct_node(
            &mut pdf,
            &scalar,
            &mut key_ancestors,
            &mut visited,
            true,
            false,
        )
        .expect("non-Pages direct node is skipped");
        push_internal(
            &mut pdf,
            ObjectRef::new(1, 0),
            &mut key_ancestors,
            &mut visited,
            true,
            false,
        )
        .expect("non-Pages indirect node is skipped");
    }
}
