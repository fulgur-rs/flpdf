//! Per-page annotation enumeration and widget-to-field linkage.
//!
//! [`enumerate_page_annotations`] reads the `/Annots` array of a leaf page,
//! resolves each annotation's `/Subtype` and `/Rect` via
//! [`AnnotationObjectHelper`], and for Widget annotations resolves the owning
//! AcroForm field object.
//!
//! [`enumerate_document_annotations`] is a convenience wrapper that applies
//! [`enumerate_page_annotations`] to every leaf page in the document.
//!
//! # Widget-to-field linkage
//!
//! A Widget annotation may be "merged" into its field (the same dictionary
//! object acts as both annotation and field, and carries `/FT`).  Alternatively
//! the widget and its field may be separate objects, with the widget holding a
//! `/Parent` reference to the field.  The linkage rule handles both cases:
//!
//! 1. If the widget dict carries a non-Null `/FT` (any type, including a
//!    Reference — its presence alone identifies a field), the widget *is* its
//!    own field: return `Some(annot_ref)`.
//! 2. Otherwise return the widget's direct `/Parent` reference — the terminal
//!    field that owns this widget.
//! 3. If the widget has neither `/FT` nor a `/Parent` reference, return `None`
//!    (orphaned widget with no traceable field).
//!
//! The owning field is deliberately the *direct* parent rather than the nearest
//! `/FT`-bearing ancestor: field attributes inherit *down* the field tree, so a
//! terminal field may omit `/FT` while an ancestor supplies it. Returning the
//! ancestor would wrongly merge several sibling terminal fields onto one ref.

use crate::page_object_helper::PageBox;
use crate::{AnnotationObjectHelper, Object, ObjectRef, PageObjectHelper, Pdf, Result};
use std::io::{Read, Seek};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// An annotation enumerated from a leaf page's `/Annots` array, together with
/// its classification and (for Widget annotations) the linked AcroForm field.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumeratedAnnotation {
    /// Indirect reference to the annotation dictionary.
    pub annot_ref: ObjectRef,

    /// The `/Subtype` name bytes, resolved from the annotation dictionary
    /// (e.g. `b"Widget"`, `b"Link"`, `b"Text"`, `b"Highlight"`).
    ///
    /// `None` when `/Subtype` is absent or not a `Name` object.
    pub subtype: Option<Vec<u8>>,

    /// The annotation bounding rectangle (`/Rect`), resolved via
    /// [`AnnotationObjectHelper::rect`].
    ///
    /// `None` when `/Rect` is absent.
    pub rect: Option<PageBox>,

    /// `true` when `subtype == Some(b"Widget")`.
    pub is_widget: bool,

    /// For Widget annotations: the [`ObjectRef`] of the AcroForm field that
    /// owns this widget.
    ///
    /// - When the widget dict itself carries `/FT` (merged field/widget), this
    ///   equals `annot_ref`.
    /// - Otherwise this is the widget's direct `/Parent` (the terminal field
    ///   that owns it).
    /// - `None` for non-Widget annotations, or for Widget annotations where no
    ///   owning field can be found.
    pub field_ref: Option<ObjectRef>,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Enumerate and classify all annotations on the leaf page identified by
/// `page_ref`.
///
/// # Algorithm
///
/// 1. Call [`PageObjectHelper::get_annotations`] to obtain the ordered list of
///    annotation [`ObjectRef`]s from `/Annots`.
/// 2. For each ref, use [`AnnotationObjectHelper`] to read `/Subtype` and
///    `/Rect`.
/// 3. Determine [`EnumeratedAnnotation::is_widget`].
/// 4. For Widget annotations, resolve the owning field (see module
///    documentation for the linkage rule).
///
/// Returns an empty `Vec` when the page has no `/Annots` entry.
///
/// # Errors
///
/// - [`crate::Error::Unsupported`] if `page_ref` does not resolve to a
///   `/Type /Page` dictionary.
/// - Any error propagated from [`Pdf::resolve`].
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{pages, Pdf};
/// use flpdf::page_annotation_enum::enumerate_page_annotations;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
/// let page_refs = pages::page_refs(&mut pdf)?;
/// if let Some(&page_ref) = page_refs.first() {
///     let annots = enumerate_page_annotations(&mut pdf, page_ref)?;
///     for a in &annots {
///         println!(
///             "annot {} subtype={:?} is_widget={}",
///             a.annot_ref,
///             a.subtype.as_deref().map(|s| String::from_utf8_lossy(s).into_owned()),
///             a.is_widget,
///         );
///     }
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn enumerate_page_annotations<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<EnumeratedAnnotation>> {
    // Step 1: obtain annotation refs (PageObjectHelper is dropped after this call).
    let annot_refs = {
        let mut page_helper = PageObjectHelper::new(page_ref, pdf);
        page_helper.get_annotations()?
    };

    let mut result = Vec::with_capacity(annot_refs.len());

    for annot_ref in annot_refs {
        // Step 2: read /Subtype and /Rect via AnnotationObjectHelper (dropped after).
        let (subtype, rect) = {
            let mut annot_helper = AnnotationObjectHelper::new(annot_ref, pdf);
            let subtype = annot_helper.subtype()?;
            let rect = annot_helper.rect()?;
            (subtype, rect)
        };

        // Step 3: classify.
        let is_widget = subtype.as_deref().is_some_and(|s| s == b"Widget");

        // Step 4: widget-to-field linkage.
        let field_ref = if is_widget {
            find_field_ref(pdf, annot_ref)?
        } else {
            None
        };

        result.push(EnumeratedAnnotation {
            annot_ref,
            subtype,
            rect,
            is_widget,
            field_ref,
        });
    }

    Ok(result)
}

/// Enumerate and classify all annotations in the document, one entry per leaf
/// page.
///
/// Returns a `Vec` of `(page_ref, annotations)` pairs in page order.
///
/// # Errors
///
/// Propagates any error from [`crate::pages::page_refs`] or
/// [`enumerate_page_annotations`].
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::Pdf;
/// use flpdf::page_annotation_enum::enumerate_document_annotations;
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("form.pdf")?))?;
/// for (page_ref, annots) in enumerate_document_annotations(&mut pdf)? {
///     println!("page {page_ref}: {} annotations", annots.len());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn enumerate_document_annotations<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Vec<(ObjectRef, Vec<EnumeratedAnnotation>)>> {
    let page_refs = crate::pages::page_refs(pdf)?;
    let mut out = Vec::with_capacity(page_refs.len());
    for page_ref in page_refs {
        let annots = enumerate_page_annotations(pdf, page_ref)?;
        out.push((page_ref, annots));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Private: widget-to-field chain walk
// ---------------------------------------------------------------------------

/// Find the AcroForm field that directly owns the widget at `start`.
///
/// Returns `Some(field_ref)` where `field_ref` is:
/// - `start` itself when the widget carries its own non-Null `/FT` (a merged
///   widget/field), OR
/// - the widget's direct `/Parent` (the *terminal* field that owns it).
///
/// The owning field is the **direct** parent, not the nearest `/FT`-bearing
/// ancestor: field type and other attributes inherit *down* the field tree, so
/// a terminal field may omit `/FT` while a higher ancestor supplies it. Walking
/// up to the first `/FT` would wrongly collapse several sibling terminal fields
/// (each inheriting `/FT` from a shared parent) onto that single ancestor.
///
/// Returns `None` when the widget has neither `/FT` nor a `/Parent` reference.
fn find_field_ref<R: Read + Seek>(pdf: &mut Pdf<R>, start: ObjectRef) -> Result<Option<ObjectRef>> {
    let node = pdf.resolve_borrowed(start)?;
    let Some(dict) = node.as_dict() else {
        return Ok(None);
    };

    // Merged widget: a non-Null /FT (review-pattern #2: it may be indirect, so
    // presence alone is enough) means the widget IS its own field.
    if matches!(dict.get("FT"), Some(v) if !matches!(v, Object::Null)) {
        return Ok(Some(start));
    }

    // Separated widget: the owning terminal field is its direct /Parent.
    match dict.get("Parent") {
        Some(Object::Reference(parent_ref)) => Ok(Some(*parent_ref)),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectRef, Pdf};
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Minimal PDF builder
    // -----------------------------------------------------------------------

    /// Build a minimal single-page PDF where the page (obj 3) has the given
    /// `/Annots` value string (already serialised, e.g. `"[4 0 R 5 0 R]"`)
    /// and any extra objects specified as `(obj_num, raw_bytes)`.
    ///
    /// Object layout: 1=Catalog, 2=Pages, 3=Page, 4..=extras.
    fn build_pdf(annots_entry: Option<&str>, extra_objects: &[(u32, &[u8])]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        let page_body = match annots_entry {
            None => "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n"
                .to_string(),
            Some(annots) => format!(
                "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Annots {annots} >>\nendobj\n"
            ),
        };
        pdf.extend_from_slice(page_body.as_bytes());

        let mut extra_offsets: Vec<(u32, u64)> = Vec::new();
        for &(num, body) in extra_objects.iter() {
            let off = pdf.len() as u64;
            extra_offsets.push((num, off));
            pdf.extend_from_slice(body);
        }

        let xref_start = pdf.len() as u64;
        let max_num = extra_offsets.iter().map(|(n, _)| *n).max().unwrap_or(3);
        let total = max_num as usize + 1;
        let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
        xref.push_str(&format!("{:010} 00000 n \n", off1));
        xref.push_str(&format!("{:010} 00000 n \n", off2));
        xref.push_str(&format!("{:010} 00000 n \n", off3));
        for i in 4u32..=max_num {
            if let Some((_, off)) = extra_offsets.iter().find(|(n, _)| *n == i) {
                xref.push_str(&format!("{:010} 00000 n \n", off));
            } else {
                xref.push_str("0000000000 65535 f \n");
            }
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    // -----------------------------------------------------------------------
    // Test: /Annots absent → empty Vec
    // -----------------------------------------------------------------------

    #[test]
    fn no_annots_returns_empty_vec() {
        let bytes = build_pdf(None, &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let result = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: mixed Widget + Link + Text → subtype classification and ordering
    // -----------------------------------------------------------------------
    //
    // Object layout:
    //   3 = Page  (/Annots [4 0 R 5 0 R 6 0 R])
    //   4 = Widget annotation (with /FT /Tx — merged)
    //   5 = Link annotation
    //   6 = Text annotation

    #[test]
    fn mixed_subtypes_are_classified_and_ordered() {
        // obj 4: Widget annotation (merged field — has /FT)
        let obj4: &[u8] =
            b"4 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [10 20 100 30] /FT /Tx >>\nendobj\n";
        // obj 5: Link annotation
        let obj5: &[u8] =
            b"5 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 100 20] >>\nendobj\n";
        // obj 6: Text annotation (no rect)
        let obj6: &[u8] = b"6 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n";

        let bytes = build_pdf(
            Some("[4 0 R 5 0 R 6 0 R]"),
            &[(4, obj4), (5, obj5), (6, obj6)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 3);

        // First: Widget
        assert_eq!(annots[0].annot_ref, ObjectRef::new(4, 0));
        assert_eq!(annots[0].subtype.as_deref(), Some(b"Widget" as &[u8]));
        assert!(annots[0].is_widget);
        // Merged widget — field_ref should be annot itself
        assert_eq!(annots[0].field_ref, Some(ObjectRef::new(4, 0)));
        assert_eq!(annots[0].rect, Some(PageBox::new(10.0, 20.0, 100.0, 30.0)));

        // Second: Link
        assert_eq!(annots[1].annot_ref, ObjectRef::new(5, 0));
        assert_eq!(annots[1].subtype.as_deref(), Some(b"Link" as &[u8]));
        assert!(!annots[1].is_widget);
        assert_eq!(annots[1].field_ref, None);

        // Third: Text (no rect)
        assert_eq!(annots[2].annot_ref, ObjectRef::new(6, 0));
        assert_eq!(annots[2].subtype.as_deref(), Some(b"Text" as &[u8]));
        assert!(!annots[2].is_widget);
        assert_eq!(annots[2].field_ref, None);
        assert_eq!(annots[2].rect, None);
    }

    // -----------------------------------------------------------------------
    // Test: merged widget — annot dict carries /FT directly
    // -----------------------------------------------------------------------

    #[test]
    fn merged_widget_field_ref_is_annot_itself() {
        // obj 4: widget annotation that is also a field (/FT present)
        let obj4: &[u8] =
            b"4 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /FT /Tx >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 1);
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, Some(ObjectRef::new(4, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: separated widget — /Parent points to field with /FT
    // -----------------------------------------------------------------------
    //
    // Object layout:
    //   4 = Widget annotation (no /FT, has /Parent 5 0 R)
    //   5 = Field dict (/FT /Tx — the owning field)

    #[test]
    fn separated_widget_field_ref_is_parent_field() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] =
            b"5 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (myfield) >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 1);
        let a = &annots[0];
        assert!(a.is_widget);
        // field_ref should point to parent (obj 5 which has /FT)
        assert_eq!(a.field_ref, Some(ObjectRef::new(5, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: multi-level chain — owning field is the DIRECT parent, not the
    // nearest /FT-bearing ancestor.
    // -----------------------------------------------------------------------
    //
    // Object layout:
    //   4 = Widget annotation (no /FT, /Parent 5 0 R)
    //   5 = Terminal field (no /FT — inherits it; /Parent 6 0 R)
    //   6 = Parent field (/FT /Btn)
    //
    // The widget's owning field is obj 5 (its direct parent), even though obj 6
    // supplies the inherited /FT.

    #[test]
    fn owning_field_is_direct_parent_not_ft_ancestor() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /Parent 6 0 R /T (option1) >>\nendobj\n";
        let obj6: &[u8] = b"6 0 obj\n<< /FT /Btn /T (radio) >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4), (5, obj5), (6, obj6)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, Some(ObjectRef::new(5, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: sibling terminal fields inheriting /FT from a shared parent must
    // map to DISTINCT field_refs (not collapsed onto the ancestor).
    // -----------------------------------------------------------------------
    //
    //   4 = Widget A (/Parent 6)   5 = Widget B (/Parent 7)
    //   6 = Terminal field A (no /FT, /Parent 8)
    //   7 = Terminal field B (no /FT, /Parent 8)
    //   8 = Parent field (/FT /Btn)

    #[test]
    fn sibling_terminal_fields_are_distinct() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 50 20] /Parent 6 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [60 0 110 20] /Parent 7 0 R >>\nendobj\n";
        let obj6: &[u8] = b"6 0 obj\n<< /Parent 8 0 R /T (a) >>\nendobj\n";
        let obj7: &[u8] = b"7 0 obj\n<< /Parent 8 0 R /T (b) >>\nendobj\n";
        let obj8: &[u8] = b"8 0 obj\n<< /FT /Btn /T (group) >>\nendobj\n";

        let bytes = build_pdf(
            Some("[4 0 R 5 0 R]"),
            &[(4, obj4), (5, obj5), (6, obj6), (7, obj7), (8, obj8)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let annots = enumerate_page_annotations(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(annots[0].field_ref, Some(ObjectRef::new(6, 0)));
        assert_eq!(annots[1].field_ref, Some(ObjectRef::new(7, 0)));
        assert_ne!(annots[0].field_ref, annots[1].field_ref);
    }

    // -----------------------------------------------------------------------
    // Test: non-Widget (Link) → field_ref is None, is_widget is false
    // -----------------------------------------------------------------------

    #[test]
    fn link_annotation_has_no_field_ref() {
        let obj4: &[u8] =
            b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 200 40] >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 1);
        let a = &annots[0];
        assert!(!a.is_widget);
        assert_eq!(a.field_ref, None);
    }

    // -----------------------------------------------------------------------
    // Test: Widget with no /FT and no /Parent → field_ref is None
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_widget_field_ref_is_none() {
        // No /FT, no /Parent — truly orphaned widget
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, None);
    }

    // -----------------------------------------------------------------------
    // Test: /Parent cycle → terminates without panic, returns Some result
    // -----------------------------------------------------------------------
    //
    // Object layout:
    //   4 = Widget (/Parent 5 0 R)
    //   5 = Intermediate (/Parent 4 0 R)  ← cycle back to 4

    #[test]
    fn cyclic_parent_chain_terminates() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /Parent 4 0 R >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        // Linkage is a single hop to the direct parent, so a /Parent cycle is
        // harmless — it never walks far enough to loop. field_ref = obj 5.
        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 1);
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, Some(ObjectRef::new(5, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: /Rect is resolved correctly
    // -----------------------------------------------------------------------

    #[test]
    fn rect_is_resolved_correctly() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Text \
                             /Rect [10 20 300 400] >>\nendobj\n";
        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots[0].rect, Some(PageBox::new(10.0, 20.0, 300.0, 400.0)));
    }

    // -----------------------------------------------------------------------
    // Test: Widget with /Parent pointing to node without /FT
    //       → topmost parent node is returned as field_ref
    // -----------------------------------------------------------------------

    #[test]
    fn widget_parent_without_ft_returns_topmost_parent() {
        // obj 4: widget (/Parent 5 0 R)
        // obj 5: field-like node, no /FT (generator omitted it)
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /T (noFT) >>\nendobj\n";

        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert!(a.is_widget);
        // No /FT anywhere — topmost parent (obj 5) is used
        assert_eq!(a.field_ref, Some(ObjectRef::new(5, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: enumerate_document_annotations covers all pages
    // -----------------------------------------------------------------------

    #[test]
    fn enumerate_document_annotations_covers_all_pages() {
        // Single page with one Text annotation
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Text >>\nendobj\n";
        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let result = enumerate_document_annotations(&mut pdf).unwrap();
        assert_eq!(result.len(), 1);
        let (pref, annots) = &result[0];
        assert_eq!(*pref, ObjectRef::new(3, 0));
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].subtype.as_deref(), Some(b"Text" as &[u8]));
    }

    // -----------------------------------------------------------------------
    // Test: page with empty /Annots array → empty Vec
    // -----------------------------------------------------------------------

    #[test]
    fn empty_annots_array_returns_empty_vec() {
        // /Annots is present but empty
        let bytes = build_pdf(Some("[]"), &[]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);
        let result = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert!(result.is_empty());
    }

    // -----------------------------------------------------------------------
    // Test: EnumeratedAnnotation implements Clone and PartialEq
    // -----------------------------------------------------------------------

    #[test]
    fn enumerated_annotation_clone_and_eq() {
        let a = EnumeratedAnnotation {
            annot_ref: ObjectRef::new(4, 0),
            subtype: Some(b"Widget".to_vec()),
            rect: Some(PageBox::new(0.0, 0.0, 100.0, 20.0)),
            is_widget: true,
            field_ref: Some(ObjectRef::new(4, 0)),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    // -----------------------------------------------------------------------
    // Test: Highlight annotation (non-Widget) → is_widget false, field_ref None
    // -----------------------------------------------------------------------

    #[test]
    fn highlight_annotation_is_not_widget() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Highlight \
                             /Rect [0 10 200 30] >>\nendobj\n";
        let bytes = build_pdf(Some("[4 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert_eq!(a.subtype.as_deref(), Some(b"Highlight" as &[u8]));
        assert!(!a.is_widget);
        assert_eq!(a.field_ref, None);
    }
}
