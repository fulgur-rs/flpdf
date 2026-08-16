//! qpdf correspondence: QPDFPageObjectHelper.cc annotation enumeration.
//! Per-page annotation enumeration and widget-to-field linkage.
//!
//! [`enumerate_page_annotations`] reads the canonical `/Annots` handle list of
//! a leaf page, projects indirect entries to the ref-based public result, and
//! resolves each annotation's `/Subtype` and `/Rect` via
//! [`AnnotationObjectHelper`], and for Widget annotations resolves the owning
//! AcroForm field object.
//!
//! [`enumerate_document_annotations`] is a convenience wrapper that applies
//! [`enumerate_page_annotations`] to every leaf page in the document.
//!
//! # Widget-to-field linkage
//!
//! Widget-to-field association follows qpdf's
//! `QPDFAcroFormDocumentHelper::getFieldForAnnotation` and
//! `QPDFFormFieldObjectHelper::getTopLevelField` composition. Matching qpdf's
//! own `analyze()` memoization (invoked lazily, only by a caller that needs a
//! field association), the `annotation_to_field_map` is built at most once per
//! [`enumerate_document_annotations`] call — shared across every page rather
//! than rebuilt per page — and only when a page actually has a Widget
//! annotation; a widget-free page never triggers it, so an unrelated
//! malformed field tree elsewhere in the document cannot fail its
//! enumeration. Each mapped field is then walked to its top-level field with
//! the helper's cycle guard. A document without an `/AcroForm` `/Fields`
//! entry therefore has no qpdf field association, while a page orphan in a
//! document with such an entry is self-mapped by qpdf's analyze pass.

use crate::page_object_helper::PageBox;
use crate::{
    AcroFormDocumentHelper, AnnotationObjectHelper, Object, ObjectRef, PageObjectHelper, Pdf,
    Result,
};
use std::collections::BTreeMap;
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
    /// [`AnnotationObjectHelper::get_rect`].
    ///
    /// `None` when `/Rect` is absent.
    pub rect: Option<PageBox>,

    /// `true` when `subtype == Some(b"Widget")`.
    pub is_widget: bool,

    /// For Widget annotations: the top-level [`ObjectRef`] of the AcroForm
    /// field that qpdf associates with this widget.
    ///
    /// `None` for non-Widget annotations, or when qpdf's AcroForm analysis has
    /// no field association for the widget.
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
/// 1. Call [`PageObjectHelper::get_annotations_filtered`] to obtain the
///    ordered indirect annotation [`ObjectRef`] projection from the canonical
///    `/Annots` handle list.
/// 2. For each ref, use [`AnnotationObjectHelper`] to read `/Subtype` and
///    `/Rect`.
/// 3. Determine [`EnumeratedAnnotation::is_widget`].
/// 4. For Widget annotations, resolve the qpdf field association and walk it
///    to the top-level field (see module documentation).
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
    let mut field_refs_by_annotation = None;
    enumerate_page_annotations_with_cache(pdf, page_ref, &mut field_refs_by_annotation)
}

/// [`enumerate_page_annotations`]'s body, taking the qpdf-analyze-equivalent
/// map as an in/out cache so [`enumerate_document_annotations`] can share one
/// map across every page instead of rebuilding it (and re-walking every page
/// for the orphan-widget pass) per page.
///
/// Mirrors qpdf's own laziness: `QPDFAcroFormDocumentHelper::analyze()` is
/// memoized on `cache_valid` and only invoked by callers (`getFieldForAnnotation`,
/// `getFormFieldsForPage`) that need a field association, so a page with zero
/// widget annotations never triggers it. Building unconditionally here would
/// mean an unrelated malformed field tree elsewhere in the document (e.g. one
/// exceeding the AcroForm depth cap) fails ordinary annotation enumeration on
/// pages that have no widgets and so never need that tree at all.
fn enumerate_page_annotations_with_cache<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    field_refs_by_annotation: &mut Option<BTreeMap<ObjectRef, ObjectRef>>,
) -> Result<Vec<EnumeratedAnnotation>> {
    // Step 1: obtain annotation refs (PageObjectHelper is dropped after this call).
    let annot_refs = {
        let mut page_helper = PageObjectHelper::new(page_ref, pdf);
        page_helper.get_annotations_filtered(None)?
    };

    let mut result = Vec::with_capacity(annot_refs.len());
    let mut has_widget = false;

    for annot_ref in annot_refs {
        // Step 2: read /Subtype and /Rect via AnnotationObjectHelper (dropped
        // after each call). /Rect presence is checked directly rather than
        // via `get_rect`'s qpdf-faithful fail-soft default (which cannot
        // distinguish an absent `/Rect` from a present-but-degenerate one),
        // preserving this module's existing "no /Rect -> None" contract for
        // its `page_annotation_flatten.rs` consumer.
        let raw_subtype = AnnotationObjectHelper::new(annot_ref, pdf).get_subtype()?;
        let subtype = (!raw_subtype.is_empty()).then_some(raw_subtype);
        let has_rect_key = pdf
            .resolve_borrowed(annot_ref)?
            .as_dict()
            .is_some_and(|d| !matches!(d.get("Rect"), None | Some(Object::Null)));
        let rect = has_rect_key
            .then(|| AnnotationObjectHelper::new(annot_ref, pdf).get_rect())
            .transpose()?;

        // Step 3: classify.
        let is_widget = subtype.as_deref().is_some_and(|s| s == b"Widget");
        has_widget |= is_widget;

        result.push(EnumeratedAnnotation {
            annot_ref,
            subtype,
            rect,
            is_widget,
            field_ref: None,
        });
    }

    // Step 4: widget-to-field linkage, deferred until a widget is actually
    // present on this page. The map contains only qpdf analyze associations;
    // non-Widgets and unassociated Widgets remain None.
    if has_widget {
        if field_refs_by_annotation.is_none() {
            *field_refs_by_annotation = Some(build_field_refs_by_annotation(pdf)?);
        }
        let map = field_refs_by_annotation
            .as_ref()
            .expect("populated immediately above");
        for annotation in &mut result {
            if annotation.is_widget {
                annotation.field_ref = map.get(&annotation.annot_ref).copied();
            }
        }
    }

    Ok(result)
}

/// Build the qpdf-`analyze()`-equivalent map from every widget annotation to
/// its top-level field: [`AcroFormDocumentHelper::annotation_to_field_map`]
/// (`getFieldForAnnotation`'s underlying association) composed with
/// [`AcroFormDocumentHelper::get_top_level_field`], matching qpdf's
/// `getFormFieldsForPage`'s `getFieldForAnnotation(annot).getTopLevelField()`.
fn build_field_refs_by_annotation<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
    let mut helper = AcroFormDocumentHelper::new(pdf);
    let annotation_to_field = helper.annotation_to_field_map()?;
    annotation_to_field
        .into_iter()
        .map(|(annot_ref, field_ref)| {
            helper
                .get_top_level_field(field_ref)
                .map(|top_level| (annot_ref, top_level))
        })
        .collect::<Result<BTreeMap<_, _>>>()
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
    // One shared, lazily-built map across every page -- matching qpdf's own
    // per-QPDFAcroFormDocumentHelper-instance memoization of analyze() --
    // instead of each page's enumeration re-walking the whole field tree and
    // every page's /Annots for the orphan-widget pass.
    let mut field_refs_by_annotation = None;
    for page_ref in page_refs {
        let annots =
            enumerate_page_annotations_with_cache(pdf, page_ref, &mut field_refs_by_annotation)?;
        out.push((page_ref, annots));
    }
    Ok(out)
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
        build_pdf_with_acroform(annots_entry, None, extra_objects)
    }

    /// Build the same fixture with an explicit AcroForm `/Fields` array on
    /// the catalog.  qpdf's analyze() only populates annotation_to_field when
    /// `/AcroForm` has a `/Fields` key; keeping this opt-in lets tests separate
    /// that contract from the no-AcroForm orphan behavior.
    fn build_pdf_with_acroform(
        annots_entry: Option<&str>,
        fields_entry: Option<&str>,
        extra_objects: &[(u32, &[u8])],
    ) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        let catalog = match fields_entry {
            Some(fields) => format!(
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields {fields} >> >>\nendobj\n"
            ),
            None => "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
        };
        pdf.extend_from_slice(catalog.as_bytes());

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

        let bytes = build_pdf_with_acroform(
            Some("[4 0 R 5 0 R 6 0 R]"),
            Some("[4 0 R]"),
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

        let bytes = build_pdf_with_acroform(Some("[4 0 R]"), Some("[4 0 R]"), &[(4, obj4)]);
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
        let obj5: &[u8] = b"5 0 obj\n<< /FT /Tx /T (myfield) /Kids [4 0 R] >>\nendobj\n";

        let bytes =
            build_pdf_with_acroform(Some("[4 0 R]"), Some("[5 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 1);
        let a = &annots[0];
        assert!(a.is_widget);
        // qpdf composes getFieldForAnnotation with getTopLevelField, so the
        // separated widget resolves to the named top-level field (obj 5).
        assert_eq!(a.field_ref, Some(ObjectRef::new(5, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: merged terminal fields that INHERIT /FT from a common ancestor must
    // resolve to themselves, not be aggregated into the ancestor (regression).
    // -----------------------------------------------------------------------
    //
    // Object layout:
    //   4 = Widget+field merged (no direct /FT, has /T, /Parent 5 0 R)
    //   6 = Widget+field merged (no direct /FT, has /T, /Parent 5 0 R)
    //   5 = Ancestor field supplying the inherited /FT (/FT /Tx, /Kids [4 6])
    //
    // Both leaves omit /FT (inherited) but carry their own /T. qpdf maps each
    // annotation to its terminal field and getTopLevelField then returns the
    // shared ancestor for this page-level form-field enumeration.

    #[test]
    fn merged_fields_inheriting_ft_are_not_aggregated_to_ancestor() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /T (field0) /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /FT /Tx /T (group) /Kids [4 0 R 6 0 R] >>\nendobj\n";
        let obj6: &[u8] = b"6 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 30 100 50] /T (field1) /Parent 5 0 R >>\nendobj\n";

        let bytes = build_pdf_with_acroform(
            Some("[4 0 R 6 0 R]"),
            Some("[5 0 R]"),
            &[(4, obj4), (5, obj5), (6, obj6)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 2);
        assert_eq!(annots[0].field_ref, Some(ObjectRef::new(5, 0)));
        assert_eq!(annots[1].field_ref, Some(ObjectRef::new(5, 0)));
    }

    #[test]
    fn unnamed_merged_widget_with_local_value_is_its_own_field() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /V (local) /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /FT /Tx /Kids [4 0 R] >>\nendobj\n";

        let bytes =
            build_pdf_with_acroform(Some("[4 0 R]"), Some("[5 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        let annots = enumerate_page_annotations(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(annots.len(), 1);
        assert_eq!(annots[0].field_ref, Some(ObjectRef::new(5, 0)));
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
        let obj5: &[u8] = b"5 0 obj\n<< /Parent 6 0 R /T (option1) /Kids [4 0 R] >>\nendobj\n";
        let obj6: &[u8] = b"6 0 obj\n<< /FT /Btn /T (radio) /Kids [5 0 R] >>\nendobj\n";

        let bytes = build_pdf_with_acroform(
            Some("[4 0 R]"),
            Some("[6 0 R]"),
            &[(4, obj4), (5, obj5), (6, obj6)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, Some(ObjectRef::new(6, 0)));
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
        let obj6: &[u8] = b"6 0 obj\n<< /Parent 8 0 R /T (a) /Kids [4 0 R] >>\nendobj\n";
        let obj7: &[u8] = b"7 0 obj\n<< /Parent 8 0 R /T (b) /Kids [5 0 R] >>\nendobj\n";
        let obj8: &[u8] = b"8 0 obj\n<< /FT /Btn /T (group) /Kids [6 0 R 7 0 R] >>\nendobj\n";

        let bytes = build_pdf_with_acroform(
            Some("[4 0 R 5 0 R]"),
            Some("[8 0 R]"),
            &[(4, obj4), (5, obj5), (6, obj6), (7, obj7), (8, obj8)],
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let annots = enumerate_page_annotations(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(annots[0].field_ref, Some(ObjectRef::new(8, 0)));
        assert_eq!(annots[1].field_ref, Some(ObjectRef::new(8, 0)));
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
    // Test: enumerate_document_annotations reuses one field-association map
    // across every page instead of rebuilding it per page.
    // -----------------------------------------------------------------------

    #[test]
    fn document_enumeration_reuses_the_field_map_across_pages() {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let off1 = bytes.len();
        bytes.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /AcroForm << /Fields [5 0 R 6 0 R] >> >>\nendobj\n",
        );
        let off2 = bytes.len();
        bytes.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
        );
        let off3 = bytes.len();
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [5 0 R] >>\nendobj\n",
        );
        let off4 = bytes.len();
        bytes.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Annots [6 0 R] >>\nendobj\n",
        );
        let off5 = bytes.len();
        bytes.extend_from_slice(
            b"5 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /FT /Tx >>\nendobj\n",
        );
        let off6 = bytes.len();
        bytes.extend_from_slice(
            b"6 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [0 0 100 20] /FT /Tx >>\nendobj\n",
        );
        let xref_start = bytes.len();
        let xref = format!(
            "xref\n0 7\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n\
             {off3:010} 00000 n \n{off4:010} 00000 n \n{off5:010} 00000 n \n{off6:010} 00000 n \n"
        );
        bytes.extend_from_slice(xref.as_bytes());
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );

        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let result = enumerate_document_annotations(&mut pdf).unwrap();
        assert_eq!(result.len(), 2);
        let (page1_ref, page1_annots) = &result[0];
        assert_eq!(*page1_ref, ObjectRef::new(3, 0));
        assert_eq!(page1_annots[0].field_ref, Some(ObjectRef::new(5, 0)));
        let (page2_ref, page2_annots) = &result[1];
        assert_eq!(*page2_ref, ObjectRef::new(4, 0));
        assert_eq!(page2_annots[0].field_ref, Some(ObjectRef::new(6, 0)));
    }

    // -----------------------------------------------------------------------
    // Test: a page with no widgets must not pay for (or fail on) an
    // unrelated malformed field tree elsewhere in the document. Matches
    // qpdf's own laziness: analyze() is only invoked by a caller that needs
    // a field association, so a page with zero widget annotations never
    // triggers it.
    // -----------------------------------------------------------------------

    #[test]
    fn widget_free_page_ignores_an_overlong_field_tree_elsewhere() {
        let obj4: &[u8] =
            b"4 0 obj\n<< /Type /Annot /Subtype /Link /Rect [0 0 200 40] >>\nendobj\n";

        let bytes = build_pdf_with_acroform(Some("[4 0 R]"), Some("[10 0 R]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();

        // Build a /Kids chain deeper than DEFAULT_MAX_ACROFORM_DEPTH, rooted
        // at 10 0 R (the sole /AcroForm /Fields entry). Traversing it would
        // error; enumerating the widget-free page must never reach it.
        let depth = crate::DEFAULT_MAX_ACROFORM_DEPTH + 5;
        for level in 0..depth {
            let this_ref = ObjectRef::new(10 + level as u32, 0);
            let mut dict = crate::Dictionary::new();
            if level + 1 < depth {
                let next_ref = ObjectRef::new(10 + level as u32 + 1, 0);
                dict.insert("Kids", Object::Array(vec![Object::Reference(next_ref)]));
            } else {
                dict.insert("FT", Object::Name(b"Tx".to_vec()));
                dict.insert("T", Object::String(b"leaf".to_vec()));
            }
            pdf.set_object(this_ref, Object::Dictionary(dict));
        }

        let page_ref = ObjectRef::new(3, 0);
        let annots = enumerate_page_annotations(&mut pdf, page_ref)
            .expect("a widget-free page must not traverse the unrelated field tree");
        assert_eq!(annots.len(), 1);
        assert!(!annots[0].is_widget);
        assert_eq!(annots[0].field_ref, None);
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
    // Test: qpdf's orphan fallback runs when /AcroForm /Fields exists
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_widget_self_maps_when_acroform_fields_exist() {
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] >>\nendobj\n";

        let bytes = build_pdf_with_acroform(Some("[4 0 R]"), Some("[]"), &[(4, obj4)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, Some(ObjectRef::new(4, 0)));
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

        let bytes =
            build_pdf_with_acroform(Some("[4 0 R]"), Some("[4 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        // qpdf's getTopLevelField follows the cycle with an ObjGen seen guard
        // and returns the node where the repeated object is observed (obj 4).
        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        assert_eq!(annots.len(), 1);
        let a = &annots[0];
        assert!(a.is_widget);
        assert_eq!(a.field_ref, Some(ObjectRef::new(4, 0)));
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
    fn widget_parent_without_ft_returns_direct_parent() {
        // obj 4: widget (/Parent 5 0 R)
        // obj 5: field-like node, no /FT (generator omitted it)
        let obj4: &[u8] = b"4 0 obj\n<< /Type /Annot /Subtype /Widget \
                             /Rect [0 0 100 20] /Parent 5 0 R >>\nendobj\n";
        let obj5: &[u8] = b"5 0 obj\n<< /T (noFT) /Kids [4 0 R] >>\nendobj\n";

        let bytes =
            build_pdf_with_acroform(Some("[4 0 R]"), Some("[5 0 R]"), &[(4, obj4), (5, obj5)]);
        let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
        let page_ref = ObjectRef::new(3, 0);

        let annots = enumerate_page_annotations(&mut pdf, page_ref).unwrap();
        let a = &annots[0];
        assert!(a.is_widget);
        // qpdf's canonical composition returns the top-level field (obj 5).
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
