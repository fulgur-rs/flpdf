//! qpdf correspondence: `QPDFJob::shouldRemoveUnreferencedResources`.
//!
//! This module owns the job-level `--remove-unreferenced-resources` policy:
//! the `auto|yes|no` mode and qpdf's shared-resource heuristic. The page/Form
//! mutation itself remains in [`crate::resources`] and is exposed through
//! [`crate::PageObjectHelper::remove_unreferenced_resources`].
//!
//! qpdf keeps these responsibilities separate. `QPDFJob` decides whether the
//! expensive page-level pass is worthwhile (`libqpdf/QPDFJob.cc:2251-2339`),
//! while `QPDFPageObjectHelper::removeUnreferencedResources` performs the
//! parse-gated `/Font` and `/XObject` mutation
//! (`libqpdf/QPDFPageObjectHelper.cc:539-649`).

use crate::object_handle::ObjectHandleIdentity;
use crate::{ObjectRef, Pdf, Result};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::io::{Read, Seek};

/// Mode passed by qpdf job-level page operations to resource pruning.
///
/// Mirrors qpdf's `--remove-unreferenced-resources=auto|yes|no`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoveUnreferencedResources {
    /// Let the qpdf job-level caller decide whether the source document warrants
    /// the per-page pruning pass.
    #[default]
    Auto,
    /// Run the canonical per-page pruning pass without the Auto heuristic.
    Yes,
    /// No-op: leave all `/Resources` entries untouched.
    No,
}

/// Decide whether qpdf's `--pages` Auto mode should run page-level resource
/// pruning for this source document.
///
/// This is qpdf 11.9.0's `QPDFJob::shouldRemoveUnreferencedResources`
/// heuristic (`libqpdf/QPDFJob.cc:2251-2337`). qpdf only pays the cost of
/// `QPDFPageObjectHelper::removeUnreferencedResources` when the source page
/// tree contains an inherited/non-leaf `/Resources`, a shared indirect
/// `/Resources` object, or a shared indirect `/XObject` dictionary. A
/// page-local indirect `/Resources` that appears once therefore returns false.
///
/// Form XObjects reachable from page `/XObject` dictionaries are traversed as
/// qpdf does, so sharing discovered in a nested Form resource scope also
/// enables the page-job pruning route.
///
/// # Errors
///
/// Returns an error when resolving the catalog, page tree, resources, or a
/// nested Form XObject fails while evaluating the heuristic.
pub fn should_remove_unreferenced_resources<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let catalog = pdf.get_object_handle(root_ref);
    let pages = pdf.resolve_to_terminal(&catalog.try_get_key(b"/Pages")?)?;
    if pages.is_null() {
        return Ok(false);
    }

    let mut queue = VecDeque::from([pages]);
    #[allow(
        clippy::mutable_key_type,
        reason = "qpdf page-job traversal intentionally keys on canonical handle identity"
    )]
    let mut nodes_seen: HashSet<ObjectHandleIdentity> = HashSet::new();
    let mut indirect_resources_seen: BTreeSet<ObjectRef> = BTreeSet::new();

    while let Some(node) = queue.pop_front() {
        let node = pdf.resolve_to_terminal(&node)?;
        if !nodes_seen.insert(node.identity_key()) {
            continue;
        }

        let dict = node.as_stream_dict().unwrap_or_else(|| node.clone());
        let kids = pdf.resolve_to_terminal(&dict.try_get_key(b"/Kids")?)?;
        if let Some(kids) = kids.try_as_array()? {
            // qpdf returns true for any non-leaf page node that owns a
            // /Resources key, even if only one descendant page is selected.
            if dict.try_has_key(b"/Resources")? {
                return Ok(true);
            }
            queue.extend(kids);
            continue;
        }

        let resources = dict.try_get_key(b"/Resources")?;
        if let Some(resources_ref) = resources.object_ref() {
            if !indirect_resources_seen.insert(resources_ref) {
                return Ok(true);
            }
        }

        let resources = pdf.resolve_to_terminal(&resources)?;
        let Some(resources_dict) = resources.as_dictionary() else {
            continue;
        };
        let xobject = resources_dict
            .get(b"/XObject".as_slice())
            .cloned()
            .unwrap_or_else(crate::ObjectHandle::null);
        if let Some(xobject_ref) = xobject.object_ref() {
            if !indirect_resources_seen.insert(xobject_ref) {
                return Ok(true);
            }
        }

        let xobject = pdf.resolve_to_terminal(&xobject)?;
        let Some(entries) = xobject.as_dictionary() else {
            continue;
        };
        for object in entries.into_values() {
            let object = pdf.resolve_to_terminal(&object)?;
            if object.is_form_xobject()? {
                queue.push_back(object);
            }
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    fn build_pdf(objects: &[(u32, &str)], root: Option<u32>) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = BTreeMap::new();
        let max = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
        for &(number, body) in objects {
            offsets.insert(number, out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }

        let xref_start = out.len() as u64;
        out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..=max {
            match offsets.get(&number) {
                Some(offset) => {
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        let trailer = match root {
            Some(root) => format!(
                "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
                max + 1
            ),
            None => format!(
                "trailer\n<< /Size {} >>\nstartxref\n{xref_start}\n%%EOF\n",
                max + 1
            ),
        };
        out.extend_from_slice(trailer.as_bytes());
        out
    }

    fn one_page_pdf(page: &str, extra: &[(u32, &str)]) -> Vec<u8> {
        let mut objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page),
        ];
        objects.extend_from_slice(extra);
        build_pdf(&objects, Some(1))
    }

    #[test]
    fn pages_auto_resource_heuristic_matches_qpdf_trigger_shapes() {
        let rootless = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, "<< /Type /Page /MediaBox [0 0 100 100] >>"),
            ],
            None,
        );
        let mut rootless = Pdf::open(Cursor::new(rootless)).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut rootless).unwrap());

        let pages_null = build_pdf(&[(1, "<< /Type /Catalog /Pages null >>")], Some(1));
        let mut pages_null = Pdf::open(Cursor::new(pages_null)).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut pages_null).unwrap());

        let duplicate_nodes = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
            ],
            Some(1),
        );
        let mut duplicate_nodes = Pdf::open(Cursor::new(duplicate_nodes)).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut duplicate_nodes).unwrap());

        let page_local = one_page_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 4 0 R >>",
            &[(4, "<< >>")],
        );
        let mut page_local = Pdf::open(Cursor::new(page_local)).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut page_local).unwrap());

        let dangling_resources = one_page_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 4 0 R >>",
            &[(6, "<< >>")],
        );
        let mut dangling_resources = Pdf::open(Cursor::new(dangling_resources)).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut dangling_resources).unwrap());

        let inherited = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (
                    2,
                    "<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>",
                ),
                (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>"),
                (4, "<< /Font << >> >>"),
            ],
            Some(1),
        );
        let mut inherited = Pdf::open(Cursor::new(inherited)).unwrap();
        assert!(should_remove_unreferenced_resources(&mut inherited).unwrap());

        let form = one_page_pdf(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources 4 0 R >>",
            &[
                (4, "<< /XObject 5 0 R >>"),
                (5, "<< /Fm 6 0 R >>"),
                (
                    6,
                    "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >> stream\n\nendstream",
                ),
            ],
        );
        let mut form = Pdf::open(Cursor::new(form)).unwrap();
        assert!(!should_remove_unreferenced_resources(&mut form).unwrap());

        let shared_xobject = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (3, "<< /Type /Page /Parent 2 0 R /Resources 5 0 R /MediaBox [0 0 100 100] >>"),
                (4, "<< /Type /Page /Parent 2 0 R /Resources 6 0 R /MediaBox [0 0 100 100] >>"),
                (5, "<< /XObject 7 0 R >>"),
                (6, "<< /XObject 7 0 R >>"),
                (7, "<< /Fm 8 0 R >>"),
                (8, "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Length 0 >> stream\n\nendstream"),
            ],
            Some(1),
        );
        let mut shared_xobject = Pdf::open(Cursor::new(shared_xobject)).unwrap();
        assert!(should_remove_unreferenced_resources(&mut shared_xobject).unwrap());

        let shared_resources = build_pdf(
            &[
                (1, "<< /Type /Catalog /Pages 2 0 R >>"),
                (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
                (
                    3,
                    "<< /Type /Page /Parent 2 0 R /Resources 5 0 R /MediaBox [0 0 100 100] >>",
                ),
                (
                    4,
                    "<< /Type /Page /Parent 2 0 R /Resources 5 0 R /MediaBox [0 0 100 100] >>",
                ),
                (5, "<< >>"),
            ],
            Some(1),
        );
        let mut shared_resources = Pdf::open(Cursor::new(shared_resources)).unwrap();
        assert!(should_remove_unreferenced_resources(&mut shared_resources).unwrap());
    }
}
