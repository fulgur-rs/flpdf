//! qpdf correspondence: QPDFAcroFormDocumentHelper.cc responsibilities shared with overlay and signature modules.
//! High-level AcroForm document helper.
//!
//! [`AcroFormDocumentHelper`] wraps a `&mut Pdf<R>` and exposes document-level
//! operations for interactive form fields. It builds on
//! [`crate::FormFieldObjectHelper`] for inherited value lookup and on
//! [`crate::copy_objects`] for cross-document field copying.

use crate::form_field_object_helper::FormFieldObjectHelper;
use crate::object::MAX_INLINE_DEPTH;
use crate::object_handle::ObjectHandle;
use crate::ref_chain::resolve_ref_chain;
use crate::{
    copy_objects, json_inspect::decode_pdf_text_string, Dictionary, Error, Object, ObjectRef, Pdf,
    Result, DEFAULT_MAX_ACROFORM_DEPTH,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

type AcroFormInheritedEntries = Vec<(Vec<u8>, Object)>;
type FieldCopySet = BTreeSet<ObjectRef>;

/// Effective metadata for one AcroForm field-tree node.
///
/// Values are materialized from the current node plus inherited field-tree
/// state. `/DA`, `/Q`, and `/MaxLen` may inherit from `/AcroForm` defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct AcroFormFieldInfo {
    /// The field dictionary object.
    pub object_ref: ObjectRef,
    /// Direct `/T` partial name bytes, when present.
    pub partial_name: Option<Vec<u8>>,
    /// Dot-joined field name path reconstructed from ancestor `/T` entries.
    pub full_name: String,
    /// Effective `/FT` field type.
    pub field_type: Option<Vec<u8>>,
    /// Effective `/V` field value.
    pub value: Option<Object>,
    /// Effective `/DV` default field value.
    pub default_value: Option<Object>,
    /// Effective `/Ff` field flags.
    pub field_flags: Option<i64>,
    /// Effective `/DA` default appearance.
    pub default_appearance: Option<Object>,
    /// Effective `/Q` quadding value.
    pub quadding: Option<i64>,
    /// Effective `/MaxLen` text-field maximum length.
    pub max_len: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct FieldInheritance {
    full_name: String,
    field_type: Option<Vec<u8>>,
    value: Option<Object>,
    default_value: Option<Object>,
    field_flags: Option<i64>,
    default_appearance: Option<Object>,
    quadding: Option<i64>,
    max_len: Option<i64>,
}

/// High-level helper for a document's `/AcroForm`.
///
/// Construct with [`AcroFormDocumentHelper::new`] or [`Pdf::acroform`]. The
/// helper holds no cached field state; methods re-read the live document so
/// prior mutations are immediately visible.
///
/// For a runnable walkthrough see `examples/list_form_fields.rs`.
pub struct AcroFormDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> AcroFormDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return all field-tree object refs in preorder.
    ///
    /// Missing `/AcroForm` or missing/malformed `/Fields` returns an empty list.
    /// Cycles are ignored after the first visit.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the catalog or a field-tree node is not a
    ///   dictionary, when an indirect `/AcroForm` reference does not resolve to a
    ///   dictionary, or when the field-tree depth limit is exceeded. A direct
    ///   non-dictionary `/AcroForm` value is ignored, not rejected.
    /// - Any error from [`Pdf::resolve`].
    pub fn fields(&mut self) -> Result<Vec<ObjectRef>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.get("Fields").cloned())? else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for item in fields {
            if let Object::Reference(field_ref) = item {
                self.walk_field_tree(field_ref, &mut seen, &mut out)?;
            }
        }
        Ok(out)
    }

    /// Return all field-tree nodes with effective inherited metadata.
    ///
    /// Missing `/AcroForm` or missing/malformed `/Fields` returns an empty
    /// list. Cycles are ignored after the first visit.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the catalog or a field-tree node is not a
    ///   dictionary, when an indirect `/AcroForm` reference does not resolve to a
    ///   dictionary, or when the field-tree depth limit is exceeded. A direct
    ///   non-dictionary `/AcroForm` value is ignored, not rejected.
    /// - Any error from [`Pdf::resolve`].
    pub fn field_infos(&mut self) -> Result<Vec<AcroFormFieldInfo>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.get("Fields").cloned())? else {
            return Ok(Vec::new());
        };

        // `?` is not usable inside a struct literal, so materialize the
        // AcroForm-default leaves (which may be indirect) into locals first.
        let default_appearance = deref_leaf(self.pdf, acroform.get("DA").cloned())?;
        let quadding = inherited_integer(self.pdf, &acroform, "Q")?;
        let max_len = inherited_integer(self.pdf, &acroform, "MaxLen")?;
        let inherited = FieldInheritance {
            default_appearance,
            quadding,
            max_len,
            ..FieldInheritance::default()
        };

        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for item in fields {
            if let Object::Reference(field_ref) = item {
                self.walk_field_info_tree(field_ref, inherited.clone(), &mut seen, &mut out, 0)?;
            }
        }
        Ok(out)
    }

    /// Build the qpdf `analyze()` annotation→field map: for every widget
    /// annotation reachable from `/AcroForm/Fields` (recursively via
    /// `/Kids`), the field dictionary that owns it, plus every widget
    /// annotation on any page not reachable that way, self-mapped as its own
    /// field (the "orphan widget" fallback).
    ///
    /// Mirrors `QPDFAcroFormDocumentHelper::analyze`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:235-286`), specifically the
    /// `annotation_to_field` half of its cache (the `field_to_annotations`/
    /// `field_to_name`/`name_to_fields` halves serve other consumers not
    /// needed here).
    ///
    /// qpdf caches this on the helper instance (`Members::cache_valid`) so
    /// repeated per-widget lookups are O(1) amortized. This helper holds no
    /// cached state (see [`Self::fields`]), so the full traversal recomputes
    /// on every call — a caller doing multiple lookups within one operation
    /// should call this once and index the returned map directly rather than
    /// calling [`Self::get_field_for_annotation`] per widget. Algorithm and
    /// output order are unchanged from qpdf; only the container (recomputed
    /// value vs. cached member) differs, and it does not change output
    /// bytes.
    ///
    /// Returns an empty map when the catalog `/AcroForm` is absent, is not a
    /// dictionary, or carries no `/Fields` key — in which case qpdf's
    /// `analyze` returns before its page `/Annots` orphan-widget pass, so
    /// that pass is skipped here too. A present-but-non-array `/Fields` is
    /// treated as empty (qpdf warns and uses an empty array), but the orphan
    /// pass below still runs in that case.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when a field-tree node is not a dictionary,
    ///   or when the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn annotation_to_field_map(&mut self) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
        let mut annotation_to_field = BTreeMap::new();

        let Some(acroform) = self.acroform_dict()? else {
            return Ok(annotation_to_field);
        };
        let Some(fields_obj) = acroform.get("Fields").cloned() else {
            return Ok(annotation_to_field);
        };
        let fields = resolve_array_value(self.pdf, Some(fields_obj))?.unwrap_or_default();

        let mut visited = BTreeSet::new();
        for item in fields {
            if let Object::Reference(field_ref) = item {
                traverse_field(
                    self.pdf,
                    field_ref,
                    None,
                    0,
                    &mut visited,
                    &mut annotation_to_field,
                )?;
            }
        }

        // Orphan page-widget pass: a /Subtype /Widget annotation reachable
        // from a page /Annots that was not associated with a field during
        // the /Fields traversal becomes its own form field.
        for page_ref in crate::pages::page_refs(self.pdf)? {
            for annot_ref in page_widget_annotation_refs(self.pdf, page_ref)? {
                annotation_to_field.entry(annot_ref).or_insert(annot_ref);
            }
        }

        Ok(annotation_to_field)
    }

    /// Return the field that owns `annot_ref`, mirroring
    /// `QPDFAcroFormDocumentHelper::getFieldForAnnotation`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:218-232`).
    ///
    /// Returns `Ok(None)` immediately, without building
    /// [`Self::annotation_to_field_map`], when `annot_ref` does not resolve
    /// to a dictionary with `/Subtype /Widget`
    /// (`QPDFObjectHandle::isDictionaryOfType("", "/Widget")`).
    ///
    /// For repeated per-widget lookups (e.g. enumerating every widget on a
    /// page or in a document), prefer building
    /// [`Self::annotation_to_field_map`] once — see its doc for why a
    /// per-widget call to this method would be O(n²) instead of qpdf's
    /// cached O(1) amortized.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::annotation_to_field_map`]'s errors.
    pub fn get_field_for_annotation(&mut self, annot_ref: ObjectRef) -> Result<Option<ObjectRef>> {
        let subtype = self
            .pdf
            .resolve_borrowed(annot_ref)?
            .as_dict()
            .and_then(|dict| dict.get("Subtype").cloned());
        // `getKey("/Subtype")` transparently dereferences on the qpdf side
        // (`QPDFObjectHandle::isDictionaryOfType`, `QPDFObjectHandle.cc:462-466`),
        // so an indirect `/Subtype` must resolve the same as a direct one here.
        let is_widget = match subtype {
            Some(value) => {
                matches!(resolve_ref_chain(self.pdf, &value)?.0, Object::Name(name) if name.as_slice() == b"Widget")
            }
            None => false,
        };
        if !is_widget {
            return Ok(None);
        }
        Ok(self.annotation_to_field_map()?.get(&annot_ref).copied())
    }

    /// Walk `/Parent` from `start` up to the top-level field — the node with
    /// no `/Parent`, or whose `/Parent` does not resolve to a dictionary via
    /// an indirect reference.
    ///
    /// Mirrors `QPDFFormFieldObjectHelper::getTopLevelField`
    /// (`libqpdf/QPDFFormFieldObjectHelper.cc:36-47`). This is
    /// [`Self::get_field_for_annotation`]'s natural composition partner: qpdf's
    /// `getFormFieldsForPage` calls `getFieldForAnnotation(annot).
    /// getTopLevelField()` for exactly this reason — `getFieldForAnnotation`
    /// alone can return a "separated" widget's own ref (see that method's
    /// doc), and callers that want the human-meaningful named field walk the
    /// rest of the way up here.
    ///
    /// Cycle-guarded on the visited [`ObjectRef`] set, returning the last
    /// node reached before a repeat — matching qpdf's `QPDFObjGen::set`
    /// guard for the common indirect-`/Parent` case. A **direct** (inline)
    /// `/Parent` value stops the walk immediately rather than being
    /// followed, unlike qpdf's `QPDFObjectHandle`-native walk, which would
    /// continue onto it — this crate has no live-identity tracking for an
    /// arbitrarily long direct-only `/Parent` chain (see
    /// [`crate::form_field_object_helper`]'s module doc for the same gap in
    /// its own `/Parent` walk). A direct `/Parent` on a field is a
    /// degenerate case that cannot come from parsing real PDF bytes (`/Parent`
    /// must be indirect for two or more kids to share a field).
    ///
    /// # Errors
    ///
    /// Any error from [`Pdf::resolve`].
    pub fn get_top_level_field(&mut self, start: ObjectRef) -> Result<ObjectRef> {
        let mut current = start;
        let mut seen = BTreeSet::new();
        while seen.insert(current) {
            let Some(dict) = self.pdf.resolve_borrowed(current)?.as_dict() else {
                break;
            };
            match dict.get("Parent") {
                Some(Object::Reference(parent_ref)) => current = *parent_ref,
                _ => break,
            }
        }
        Ok(current)
    }

    /// Return the field's inherited `/V` value.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when a field-tree node is not a dictionary, or
    ///   when the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn field_value(&mut self, field_ref: ObjectRef) -> Result<Option<ObjectHandle>> {
        FormFieldObjectHelper::new(field_ref, self.pdf).field_value()
    }

    /// Set the field's direct `/V` value.
    ///
    /// This updates the field dictionary itself. It does not synthesize widget
    /// appearance streams.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `field_ref` does not resolve to a
    ///   dictionary.
    /// - Any error from [`Pdf::resolve`].
    pub fn set_field_value(&mut self, field_ref: ObjectRef, value: ObjectHandle) -> Result<()> {
        FormFieldObjectHelper::new(field_ref, self.pdf).set_field_attribute(b"V", value)
    }

    /// Set `/AcroForm/DA`, creating `/AcroForm` if needed.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the document has no `/Root`.
    /// - [`Error::Unsupported`] when the catalog or `/AcroForm` does not resolve
    ///   to a dictionary, or when the object-number space is exhausted while
    ///   creating `/AcroForm`.
    /// - Any error from [`Pdf::resolve`].
    pub fn set_default_appearance(&mut self, appearance: Vec<u8>) -> Result<()> {
        let acroform_ref = self.ensure_acroform_ref()?;
        let mut acroform = self.resolve_dict(acroform_ref, "AcroForm")?;
        acroform.insert("DA", Object::String(appearance));
        self.pdf
            .set_object(acroform_ref, Object::Dictionary(acroform));
        Ok(())
    }

    /// Copy `/AcroForm/DA` onto fields that do not carry a direct `/DA`.
    ///
    /// Existing field-level `/DA` values are preserved.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the catalog or a field-tree node is not a
    ///   dictionary, when an indirect `/AcroForm` reference does not resolve to a
    ///   dictionary, or when the field-tree depth limit is exceeded. A direct
    ///   non-dictionary `/AcroForm` value is ignored, not rejected.
    /// - Any error from [`Pdf::resolve`].
    pub fn fix_appearance_inheritance(&mut self) -> Result<()> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(());
        };
        let Some(da) = acroform.get("DA").cloned() else {
            return Ok(());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.get("Fields").cloned())? else {
            return Ok(());
        };

        let mut seen = BTreeSet::new();
        for item in fields {
            if let Object::Reference(field_ref) = item {
                self.fix_field_appearance_inheritance(
                    field_ref,
                    &da,
                    &BTreeMap::new(),
                    &mut seen,
                    0,
                )?;
            }
        }
        Ok(())
    }

    /// Copy all top-level fields from `source` and append them to this document.
    ///
    /// Returns the copied top-level field refs in the target document.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the target document has no `/Root`.
    /// - [`Error::Unsupported`] when the catalog or a field-tree node is not a
    ///   dictionary, when an indirect `/AcroForm` reference does not resolve to a
    ///   dictionary, when a depth limit (field-tree or reference-chain) is
    ///   exceeded, or when the target object-number space is exhausted. A direct
    ///   non-dictionary `/AcroForm` value is ignored, not rejected.
    /// - Any error propagated from [`copy_objects`] (for example a failed
    ///   [`Pdf::resolve`] on `source`).
    pub fn copy_fields_from<RS: Read + Seek>(
        &mut self,
        source: &mut Pdf<RS>,
    ) -> Result<Vec<ObjectRef>> {
        let (top_fields, inherited_entries, copy_set) = source_field_copy_set(source)?;
        if top_fields.is_empty() {
            return Ok(Vec::new());
        }

        let map = copy_objects(source, self.pdf, &copy_set)?;
        let copied_top: Vec<ObjectRef> = top_fields
            .iter()
            .filter_map(|field_ref| map.get(field_ref).copied())
            .collect();

        let acroform_ref = self.ensure_acroform_ref()?;
        let mut acroform = self.resolve_dict(acroform_ref, "AcroForm")?;
        let mut fields =
            resolve_array_value(self.pdf, acroform.get("Fields").cloned())?.unwrap_or_default();
        fields.extend(copied_top.iter().copied().map(Object::Reference));
        acroform.insert("Fields", Object::Array(fields));

        let mut source_da = None;
        let mut source_dr = None;
        for (key, value) in inherited_entries {
            let mapped = remap_refs_in_object(value, &map);
            match key.as_slice() {
                b"DA" => {
                    source_da = Some(mapped);
                }
                b"DR" => {
                    source_dr = Some(mapped);
                }
                _ => {}
            }
        }
        materialize_acroform_dr(&mut acroform, self.pdf)?;
        let font_renames = match source_dr {
            Some(dr) => {
                let dr = resolve_dictionary_object(self.pdf, dr)?;
                let dr = materialize_resource_categories_in_object(self.pdf, dr)?;
                merge_acroform_dr(&mut acroform, dr)
            }
            None => BTreeMap::new(),
        };
        let source_da = source_da.map(|da| rewrite_da_resource_names(da, &font_renames));
        if let Some(da) = source_da.clone() {
            if acroform.get("DA").is_none() {
                acroform.insert("DA", da);
            }
        }
        self.pdf
            .set_object(acroform_ref, Object::Dictionary(acroform));

        if let Some(da) = source_da {
            let mut seen = BTreeSet::new();
            for copied_ref in &copied_top {
                self.fix_field_appearance_inheritance(
                    *copied_ref,
                    &da,
                    &font_renames,
                    &mut seen,
                    0,
                )?;
            }
        } else if !font_renames.is_empty() {
            let mut seen = BTreeSet::new();
            for copied_ref in &copied_top {
                self.rewrite_field_da_resource_names(*copied_ref, &font_renames, &mut seen, 0)?;
            }
        }

        Ok(copied_top)
    }

    fn acroform_ref(&mut self) -> Result<Option<ObjectRef>> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.resolve_dict(root_ref, "catalog")?;
        Ok(catalog.get_ref("AcroForm"))
    }

    fn acroform_dict(&mut self) -> Result<Option<Dictionary>> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.resolve_dict(root_ref, "catalog")?;
        match catalog.get("AcroForm").cloned() {
            None | Some(Object::Null) => Ok(None),
            Some(Object::Dictionary(dict)) => Ok(Some(dict)),
            // qpdf's `analyze()` (`QPDFAcroFormDocumentHelper.cc:241-243`)
            // reads `/AcroForm` through `getKey`, which transparently
            // dereferences, then treats a non-dictionary result as absent
            // with no warning, regardless of whether the source was direct
            // or indirect. `resolve_dict`'s hard `Err` on a non-dictionary
            // target is the right contract for callers that already know
            // they hold a field/annotation reference, but not here: this
            // method's own direct-value arms above already return `None`
            // for a malformed `/AcroForm`, and an indirect reference to the
            // same malformed shapes must degrade the same way.
            Some(Object::Reference(acroform_ref)) => {
                match self.pdf.resolve_borrowed(acroform_ref)? {
                    Object::Dictionary(dict) => Ok(Some(dict.clone())),
                    _ => Ok(None),
                }
            }
            Some(_) => Ok(None),
        }
    }

    pub(crate) fn ensure_acroform_ref(&mut self) -> Result<ObjectRef> {
        if let Some(existing_ref) = self.acroform_ref()? {
            return Ok(existing_ref);
        }

        let root_ref = self.pdf.root_ref().ok_or(Error::Missing("/Root"))?;
        let mut catalog = self.resolve_dict(root_ref, "catalog")?;
        let new_ref = self.next_object_ref()?;
        let acroform = match catalog.get("AcroForm").cloned() {
            Some(Object::Dictionary(dict)) => dict,
            _ => {
                let mut dict = Dictionary::new();
                dict.insert("Fields", Object::Array(Vec::new()));
                dict
            }
        };

        catalog.insert("AcroForm", Object::Reference(new_ref));
        self.pdf.set_object(new_ref, Object::Dictionary(acroform));
        self.pdf.set_object(root_ref, Object::Dictionary(catalog));
        Ok(new_ref)
    }

    fn next_object_ref(&self) -> Result<ObjectRef> {
        let next = self
            .pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
        Ok(ObjectRef::new(next, 0))
    }

    fn resolve_dict(&mut self, object_ref: ObjectRef, label: &str) -> Result<Dictionary> {
        match self.pdf.resolve_borrowed(object_ref)? {
            Object::Dictionary(dict) => Ok(dict.clone()),
            _ => Err(Error::Unsupported(format!(
                "{label} object {object_ref} is not a dictionary"
            ))),
        }
    }

    fn resolve_field_dict(&mut self, field_ref: ObjectRef) -> Result<Dictionary> {
        self.resolve_dict(field_ref, "field")
    }

    fn walk_field_tree(
        &mut self,
        field_ref: ObjectRef,
        seen: &mut BTreeSet<ObjectRef>,
        out: &mut Vec<ObjectRef>,
    ) -> Result<()> {
        self.walk_field_tree_rec(field_ref, seen, out, 0)
    }

    fn walk_field_tree_rec(
        &mut self,
        field_ref: ObjectRef,
        seen: &mut BTreeSet<ObjectRef>,
        out: &mut Vec<ObjectRef>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Err(Error::Unsupported(format!(
                "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
            )));
        }
        if !seen.insert(field_ref) {
            return Ok(());
        }
        out.push(field_ref);

        let field = self.resolve_field_dict(field_ref)?;
        let Some(kids) = resolve_array_value(self.pdf, field.get("Kids").cloned())? else {
            return Ok(());
        };
        for kid in kids {
            if let Object::Reference(kid_ref) = kid {
                self.walk_field_tree_rec(kid_ref, seen, out, depth + 1)?;
            }
        }
        Ok(())
    }

    fn walk_field_info_tree(
        &mut self,
        field_ref: ObjectRef,
        inherited: FieldInheritance,
        seen: &mut BTreeSet<ObjectRef>,
        out: &mut Vec<AcroFormFieldInfo>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Err(Error::Unsupported(format!(
                "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
            )));
        }
        if !seen.insert(field_ref) {
            return Ok(());
        }

        let field = self.resolve_field_dict(field_ref)?;
        if is_pure_widget_annotation(&field) {
            return Ok(());
        }
        let current = inherited.apply(self.pdf, &field)?;
        let partial_name = deref_leaf(self.pdf, field.get("T").cloned())?
            .as_ref()
            .and_then(Object::as_string)
            .map(|name| name.to_vec());

        out.push(AcroFormFieldInfo {
            object_ref: field_ref,
            partial_name,
            full_name: current.full_name.clone(),
            field_type: current.field_type.clone(),
            value: current.value.clone(),
            default_value: current.default_value.clone(),
            field_flags: current.field_flags,
            default_appearance: current.default_appearance.clone(),
            quadding: current.quadding,
            max_len: current.max_len,
        });

        let Some(kids) = resolve_array_value(self.pdf, field.get("Kids").cloned())? else {
            return Ok(());
        };
        for kid in kids {
            if let Object::Reference(kid_ref) = kid {
                self.walk_field_info_tree(kid_ref, current.clone(), seen, out, depth + 1)?;
            }
        }
        Ok(())
    }

    fn fix_field_appearance_inheritance(
        &mut self,
        field_ref: ObjectRef,
        inherited_da: &Object,
        font_renames: &BTreeMap<Vec<u8>, Vec<u8>>,
        seen: &mut BTreeSet<ObjectRef>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Err(Error::Unsupported(format!(
                "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
            )));
        }
        if !seen.insert(field_ref) {
            return Ok(());
        }

        let mut field = self.resolve_field_dict(field_ref)?;
        let current_da = match field.get("DA").cloned() {
            Some(da) => {
                let rewritten = rewrite_da_resource_names(da, font_renames);
                if field.get("DA") != Some(&rewritten) {
                    field.insert("DA", rewritten.clone());
                    self.pdf
                        .set_object(field_ref, Object::Dictionary(field.clone()));
                }
                rewritten
            }
            None => {
                field.insert("DA", inherited_da.clone());
                self.pdf
                    .set_object(field_ref, Object::Dictionary(field.clone()));
                inherited_da.clone()
            }
        };

        let Some(kids) = resolve_array_value(self.pdf, field.get("Kids").cloned())? else {
            return Ok(());
        };
        for kid in kids {
            if let Object::Reference(kid_ref) = kid {
                self.fix_field_appearance_inheritance(
                    kid_ref,
                    &current_da,
                    font_renames,
                    seen,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }

    fn rewrite_field_da_resource_names(
        &mut self,
        field_ref: ObjectRef,
        font_renames: &BTreeMap<Vec<u8>, Vec<u8>>,
        seen: &mut BTreeSet<ObjectRef>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Err(Error::Unsupported(format!(
                "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
            )));
        }
        if !seen.insert(field_ref) {
            return Ok(());
        }

        let mut field = self.resolve_field_dict(field_ref)?;
        if let Some(da) = field.get("DA").cloned() {
            let rewritten = rewrite_da_resource_names(da, font_renames);
            if field.get("DA") != Some(&rewritten) {
                field.insert("DA", rewritten);
                self.pdf
                    .set_object(field_ref, Object::Dictionary(field.clone()));
            }
        }

        let Some(kids) = resolve_array_value(self.pdf, field.get("Kids").cloned())? else {
            return Ok(());
        };
        for kid in kids {
            if let Object::Reference(kid_ref) = kid {
                self.rewrite_field_da_resource_names(kid_ref, font_renames, seen, depth + 1)?;
            }
        }
        Ok(())
    }
}

impl FieldInheritance {
    fn apply<R: Read + Seek>(&self, pdf: &mut Pdf<R>, field: &Dictionary) -> Result<Self> {
        let partial_name = deref_leaf(pdf, field.get("T").cloned())?
            .as_ref()
            .and_then(Object::as_string)
            .map(decode_field_name);
        let full_name = match (self.full_name.is_empty(), partial_name.as_deref()) {
            (_, None) => self.full_name.clone(),
            (true, Some(name)) => name.to_string(),
            (false, Some(name)) => format!("{}.{}", self.full_name, name),
        };

        Ok(Self {
            full_name,
            field_type: inherited_name(pdf, field, "FT")?.or_else(|| self.field_type.clone()),
            value: inherited_object(pdf, field, "V")?.or_else(|| self.value.clone()),
            default_value: inherited_object(pdf, field, "DV")?
                .or_else(|| self.default_value.clone()),
            field_flags: inherited_integer(pdf, field, "Ff")?.or(self.field_flags),
            default_appearance: inherited_object(pdf, field, "DA")?
                .or_else(|| self.default_appearance.clone()),
            quadding: inherited_integer(pdf, field, "Q")?.or(self.quadding),
            max_len: inherited_integer(pdf, field, "MaxLen")?.or(self.max_len),
        })
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level AcroForm helper for this document.
    pub fn acroform(&mut self) -> AcroFormDocumentHelper<'_, R> {
        AcroFormDocumentHelper::new(self)
    }
}

/// Resolve one level of indirection for a metadata leaf value. A resolved
/// `Object::Null` (freed/unknown ref) is treated as absent to match
/// `inherited_object`'s existing Null handling. A direct (non-reference)
/// value passes through unchanged, so this is a no-op for already-materialized
/// PDFs.
fn deref_leaf<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<Option<Object>> {
    match value {
        Some(Object::Reference(object_ref)) => match pdf.resolve(object_ref)? {
            Object::Null => Ok(None),
            resolved => Ok(Some(resolved)),
        },
        other => Ok(other),
    }
}

fn inherited_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: &Dictionary,
    key: &str,
) -> Result<Option<Object>> {
    match deref_leaf(pdf, field.get(key).cloned())? {
        Some(Object::Null) | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

fn inherited_name<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: &Dictionary,
    key: &str,
) -> Result<Option<Vec<u8>>> {
    Ok(deref_leaf(pdf, field.get(key).cloned())?
        .as_ref()
        .and_then(Object::as_name)
        .map(|name| name.to_vec()))
}

fn inherited_integer<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: &Dictionary,
    key: &str,
) -> Result<Option<i64>> {
    Ok(deref_leaf(pdf, field.get(key).cloned())?
        .as_ref()
        .and_then(Object::as_integer))
}

fn is_pure_widget_annotation(field: &Dictionary) -> bool {
    let is_widget = matches!(
        field.get("Subtype"),
        Some(Object::Name(name)) if name.as_slice() == b"Widget"
    );
    let has_field_entries = field.get("T").is_some()
        || field.get("FT").is_some()
        || field.get("Kids").is_some()
        || field.get("V").is_some()
        || field.get("DV").is_some()
        || field.get("Ff").is_some()
        || field.get("TU").is_some()
        || field.get("TM").is_some()
        || field.get("DA").is_some()
        || field.get("Q").is_some()
        || field.get("MaxLen").is_some();

    is_widget && !has_field_entries
}

fn decode_field_name(name: &[u8]) -> String {
    decode_pdf_text_string(name).unwrap_or_else(|| String::from_utf8_lossy(name).into_owned())
}

fn source_field_copy_set<RS: Read + Seek>(
    source: &mut Pdf<RS>,
) -> Result<(Vec<ObjectRef>, AcroFormInheritedEntries, FieldCopySet)> {
    let mut helper = AcroFormDocumentHelper::new(source);
    let top_fields = helper.top_level_fields()?;
    let inherited_entries = helper.acroform_inherited_entries()?;
    let mut copy_set = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for field_ref in &top_fields {
        // Field-tree walk: skip a widget's /P (its page back-pointer) so the
        // closure never pulls the page and its sibling tree into the copy set.
        collect_reachable_refs(helper.pdf, *field_ref, &mut copy_set, &mut seen, 0, true)?;
    }
    for (_, value) in &inherited_entries {
        // /DR and /DA are resource subtrees, not field-tree nodes: a resource may
        // be legitimately named /P (e.g. a /DA-referenced font), so collect /P
        // here rather than dropping it as a field-tree back-pointer. A well-formed
        // resource dict holds no field-tree back-pointers; the `seen` set and the
        // depth cap still bound traversal against cycles and long reference chains
        // (DoS) on hostile input.
        collect_refs_in_object(helper.pdf, value, &mut copy_set, &mut seen, 0, 0, false)?;
    }
    Ok((top_fields, inherited_entries, copy_set))
}

impl<'a, R: Read + Seek> AcroFormDocumentHelper<'a, R> {
    pub(crate) fn top_level_fields(&mut self) -> Result<Vec<ObjectRef>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.get("Fields").cloned())? else {
            return Ok(Vec::new());
        };
        Ok(fields
            .into_iter()
            .filter_map(|item| match item {
                Object::Reference(field_ref) => Some(field_ref),
                _ => None,
            })
            .collect())
    }

    pub(crate) fn acroform_inherited_entries(&mut self) -> Result<Vec<(Vec<u8>, Object)>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        Ok([b"DA".as_slice(), b"DR".as_slice()]
            .into_iter()
            .filter_map(|key| {
                acroform
                    .get(key)
                    .cloned()
                    .map(|value| (key.to_vec(), value))
            })
            .collect())
    }
}

pub(crate) fn collect_reachable_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object_ref: ObjectRef,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    skip_parent_key: bool,
) -> Result<()> {
    // The `seen` cycle guard cannot stop a long *acyclic* indirect-reference chain
    // (obj1 -> obj2 -> ... -> objN), where recursion depth grows with the chain length.
    // Bound the reference chain to avoid stack overflow on hostile source PDFs. Two
    // independent recursion axes are bounded separately: the `depth` parameter bounds the
    // indirect-reference-hop axis (`DEFAULT_MAX_ACROFORM_DEPTH`), incremented once per
    // resolved reference; inline structural nesting within a single resolved object is
    // bounded by the `inline_depth`/`MAX_INLINE_DEPTH` axis (see `collect_refs_in_object`),
    // reset to 0 at each ref hop because a freshly resolved object starts a new inline walk.
    if depth > DEFAULT_MAX_ACROFORM_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm reference chain depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
        )));
    }
    if !seen.insert(object_ref) {
        return Ok(());
    }
    out.insert(object_ref);

    let obj = pdf.resolve(object_ref)?;
    collect_refs_in_object(pdf, &obj, out, seen, depth, 0, skip_parent_key)
}

pub(crate) fn collect_refs_in_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    inline_depth: usize,
    skip_parent_key: bool,
) -> Result<()> {
    if inline_depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(object_ref) => {
            // Ref hop: bump the ref-hop axis; `collect_reachable_refs` resets
            // `inline_depth` to 0 for the freshly resolved object.
            collect_reachable_refs(pdf, *object_ref, out, seen, depth + 1, skip_parent_key)
        }
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(
                    pdf,
                    item,
                    out,
                    seen,
                    depth,
                    inline_depth + 1,
                    skip_parent_key,
                )?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => collect_refs_in_dict(
            pdf,
            dict,
            out,
            seen,
            depth,
            inline_depth + 1,
            skip_parent_key,
        ),
        Object::Stream(stream) => collect_refs_in_dict(
            pdf,
            &stream.dict,
            out,
            seen,
            depth,
            inline_depth + 1,
            skip_parent_key,
        ),
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::RealLiteral { .. }
        | Object::Name(_)
        | Object::String(_)
        | Object::Operator(_)
        | Object::InlineImage(_) => Ok(()),
    }
}

fn collect_refs_in_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    inline_depth: usize,
    skip_parent_key: bool,
) -> Result<()> {
    for (key, value) in dict.iter() {
        // Skip /P while it is a page back-pointer (`skip_parent_key` tracks that
        // context; see the `next_skip_parent_key` derivation below). Inside
        // resource data /P is an ordinary resource name and must be collected.
        if skip_parent_key && key == b"P" {
            continue;
        }
        // /P is a page back-pointer throughout the annotation/field graph — field
        // and widget dicts, but also nested annotations reached via non-field keys
        // (e.g. a widget's /Popup, whose own /P points at a page). Keep skipping it
        // across that whole graph. It only becomes an ordinary resource name once
        // the walk crosses into resource data via /Resources (an appearance
        // stream's, page's, or XObject's resources — e.g. a font named /P), so the
        // skip is lifted there and stays off for that resource subtree. (The
        // inherited /DR /DA walk already enters with the skip off; see the call in
        // `source_field_copy_set`.)
        let next_skip_parent_key = skip_parent_key && key != b"Resources";
        // Forward the same `inline_depth`: the caller incremented it when
        // descending into this dict, and each value re-enters
        // `collect_refs_in_object` where the guard re-checks.
        collect_refs_in_object(
            pdf,
            value,
            out,
            seen,
            depth,
            inline_depth,
            next_skip_parent_key,
        )?;
    }
    Ok(())
}

fn resolve_array_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Option<Object>,
) -> Result<Option<Vec<Object>>> {
    match value {
        None | Some(Object::Null) => Ok(None),
        Some(Object::Array(values)) => Ok(Some(values)),
        Some(value @ Object::Reference(_)) => {
            // The array carrier itself may be a holder chain (`/Fields 20 0 R →
            // 21 0 R → [..]`); follow it to the terminal so a doubled-indirect
            // carrier yields its array instead of being dropped as a non-array.
            // The terminal is returned by value, so the array moves out without
            // the prior `.clone()`.
            match resolve_ref_chain(pdf, &value)?.0 {
                Object::Array(values) => Ok(Some(values)),
                _ => Ok(None), // Null or non-array terminal
            }
        }
        Some(_) => Ok(None),
    }
}

fn resolve_dictionary_object<R: Read + Seek>(pdf: &mut Pdf<R>, obj: Object) -> Result<Object> {
    match obj {
        Object::Reference(object_ref) => match pdf.resolve_borrowed(object_ref)? {
            Object::Dictionary(dict) => Ok(Object::Dictionary(dict.clone())),
            _ => Ok(Object::Reference(object_ref)),
        },
        other => Ok(other),
    }
}

fn materialize_acroform_dr<R: Read + Seek>(
    acroform: &mut Dictionary,
    pdf: &mut Pdf<R>,
) -> Result<()> {
    let Some(dr) = acroform.get("DR").cloned() else {
        return Ok(());
    };
    let dr = resolve_dictionary_object(pdf, dr)?;
    acroform.insert("DR", materialize_resource_categories_in_object(pdf, dr)?);
    Ok(())
}

pub(crate) fn remap_refs_in_object(obj: Object, map: &BTreeMap<ObjectRef, ObjectRef>) -> Object {
    match obj {
        Object::Reference(object_ref) => map
            .get(&object_ref)
            .copied()
            .map(Object::Reference)
            .unwrap_or(Object::Null),
        Object::Array(items) => Object::Array(
            items
                .into_iter()
                .map(|item| remap_refs_in_object(item, map))
                .collect(),
        ),
        Object::Dictionary(dict) => Object::Dictionary(remap_refs_in_dict(dict, map)),
        Object::Stream(mut stream) => {
            stream.dict = remap_refs_in_dict(stream.dict, map);
            Object::Stream(stream)
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::RealLiteral { .. }
        | Object::Name(_)
        | Object::String(_)
        | Object::Operator(_)
        | Object::InlineImage(_) => obj,
    }
}

fn remap_refs_in_dict(dict: Dictionary, map: &BTreeMap<ObjectRef, ObjectRef>) -> Dictionary {
    let mut out = Dictionary::new();
    for (key, value) in dict.iter() {
        out.insert(key, remap_refs_in_object(value.clone(), map));
    }
    out
}

fn merge_acroform_dr(acroform: &mut Dictionary, source_dr: Object) -> BTreeMap<Vec<u8>, Vec<u8>> {
    match acroform.remove("DR") {
        None | Some(Object::Null) => {
            acroform.insert("DR", source_dr);
            BTreeMap::new()
        }
        Some(Object::Dictionary(target_dr)) => {
            if let Object::Dictionary(source_dr) = source_dr {
                let (merged, renames) = merge_resource_dicts(target_dr, source_dr);
                acroform.insert("DR", Object::Dictionary(merged));
                renames
            } else {
                acroform.insert("DR", Object::Dictionary(target_dr));
                BTreeMap::new()
            }
        }
        Some(existing) => {
            acroform.insert("DR", existing);
            BTreeMap::new()
        }
    }
}

fn materialize_resource_categories_in_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dr: Object,
) -> Result<Object> {
    match dr {
        Object::Dictionary(mut dict) => {
            materialize_resource_categories(&mut dict, pdf)?;
            Ok(Object::Dictionary(dict))
        }
        other => Ok(other),
    }
}

fn materialize_resource_categories<R: Read + Seek>(
    dr: &mut Dictionary,
    pdf: &mut Pdf<R>,
) -> Result<()> {
    let categories: Vec<Vec<u8>> = dr.iter().map(|(key, _)| key.to_vec()).collect();
    for category in categories {
        let Some(value) = dr.get(&category).cloned() else {
            continue;
        };
        dr.insert(&category, resolve_dictionary_object(pdf, value)?);
    }
    Ok(())
}

fn merge_resource_dicts(
    mut target: Dictionary,
    source: Dictionary,
) -> (Dictionary, BTreeMap<Vec<u8>, Vec<u8>>) {
    let mut font_renames = BTreeMap::new();
    for (category, source_value) in source.iter() {
        match (target.remove(category), source_value) {
            (None, _) => target.insert(category, source_value.clone()),
            (Some(Object::Dictionary(target_category)), Object::Dictionary(source_category)) => {
                let (merged, renames) =
                    merge_resource_category(target_category, source_category, category == b"Font");
                if category == b"Font" {
                    font_renames.extend(renames);
                }
                target.insert(category, Object::Dictionary(merged));
            }
            (Some(existing), _) => target.insert(category, existing),
        }
    }
    (target, font_renames)
}

fn merge_resource_category(
    mut target: Dictionary,
    source: &Dictionary,
    rename_conflicts: bool,
) -> (Dictionary, BTreeMap<Vec<u8>, Vec<u8>>) {
    let mut renames = BTreeMap::new();
    for (name, value) in source.iter() {
        match target.get(name) {
            None => target.insert(name, value.clone()),
            Some(existing) if existing == value => {}
            Some(_) if rename_conflicts => {
                let renamed = unique_resource_name(name, &target);
                target.insert(&renamed, value.clone());
                renames.insert(name.to_vec(), renamed);
            }
            Some(_) => {}
        }
    }
    (target, renames)
}

fn unique_resource_name(base: &[u8], existing: &Dictionary) -> Vec<u8> {
    let mut candidate = [base, b"_flpdf"].concat();
    let mut suffix = 2u32;
    while existing.get(&candidate).is_some() {
        candidate = [base, b"_flpdf", suffix.to_string().as_bytes()].concat();
        suffix += 1;
    }
    candidate
}

fn rewrite_da_resource_names(da: Object, renames: &BTreeMap<Vec<u8>, Vec<u8>>) -> Object {
    if renames.is_empty() {
        return da;
    }
    match da {
        Object::String(bytes) => Object::String(rewrite_pdf_name_tokens(&bytes, renames)),
        other => other,
    }
}

fn rewrite_pdf_name_tokens(bytes: &[u8], renames: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'/' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }

        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && !is_pdf_name_delimiter(bytes[end]) {
            end += 1;
        }
        out.push(b'/');
        if let Some(renamed) = renames.get(&bytes[start..end]) {
            out.extend_from_slice(renamed);
        } else {
            out.extend_from_slice(&bytes[start..end]);
        }
        i = end;
    }
    out
}

fn is_pdf_name_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// One node of qpdf's `traverseField`: classify `field_ref` as a field
/// and/or annotation, recording the owning form-field ref when it is an
/// annotation, and recurse into an array `/Kids`.
///
/// Mirrors `libqpdf/QPDFAcroFormDocumentHelper.cc:288-362`.
fn traverse_field<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field_ref: ObjectRef,
    parent_ref: Option<ObjectRef>,
    depth: usize,
    visited: &mut BTreeSet<ObjectRef>,
    annotation_to_field: &mut BTreeMap<ObjectRef, ObjectRef>,
) -> Result<()> {
    if depth > DEFAULT_MAX_ACROFORM_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH} at {field_ref}"
        )));
    }
    // Non-dictionary fields/annotations are ignored (qpdf warns and skips).
    let Some(dict) = pdf.resolve_borrowed(field_ref)?.as_dict().cloned() else {
        return Ok(());
    };
    // Loop guard, keyed on the object ref (qpdf's ObjGen visited set).
    if !visited.insert(field_ref) {
        return Ok(());
    }

    // A terminal field that looks like an annotation is an annotation
    // (merged widget/field). A node with an array /Kids groups sub-fields
    // instead.
    let mut is_annotation = false;
    let mut is_field = depth == 0;

    if let Some(kids) = resolve_array_value(pdf, dict.get("Kids").cloned())? {
        is_field = true;
        for kid in kids {
            if let Object::Reference(kid_ref) = kid {
                traverse_field(
                    pdf,
                    kid_ref,
                    Some(field_ref),
                    depth + 1,
                    visited,
                    annotation_to_field,
                )?;
            }
        }
    } else {
        if dict.get("Parent").is_some() {
            is_field = true;
        }
        if dict.get("Subtype").is_some() || dict.get("Rect").is_some() || dict.get("AP").is_some() {
            is_annotation = true;
        }
    }

    if is_annotation {
        // our_field = is_field ? field : parent. `is_field` is false only
        // when depth > 0, where the caller always supplies a parent, so the
        // fallback is never reached.
        let our_field = if is_field {
            field_ref
        } else {
            parent_ref.unwrap_or(field_ref)
        };
        annotation_to_field.insert(field_ref, our_field);
    }

    Ok(())
}

/// Return the object refs of the `/Subtype /Widget` annotations in a leaf
/// page's `/Annots` array, mirroring qpdf's `getWidgetAnnotationsForPage`
/// (`QPDFPageObjectHelper::getAnnotations("/Widget")`). An indirect
/// `/Annots` array is resolved; non-reference entries are skipped.
fn page_widget_annotation_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    let annots_value = pdf
        .resolve_borrowed(page_ref)?
        .as_dict()
        .and_then(|page| page.get("Annots").cloned());
    let Some(annots) = resolve_array_value(pdf, annots_value)? else {
        return Ok(Vec::new());
    };

    let mut widgets = Vec::new();
    for annot in annots {
        let Object::Reference(annot_ref) = annot else {
            continue;
        };
        let subtype = pdf
            .resolve_borrowed(annot_ref)?
            .as_dict()
            .and_then(|dict| dict.get("Subtype").cloned());
        // See `get_field_for_annotation`'s doc: `/Subtype` must resolve the same
        // whether stored direct or indirect (`QPDFObjectHandle.cc:462-466`).
        let is_widget = match subtype {
            Some(value) => {
                matches!(resolve_ref_chain(pdf, &value)?.0, Object::Name(name) if name.as_slice() == b"Widget")
            }
            None => false,
        };
        if is_widget {
            widgets.push(annot_ref);
        }
    }
    Ok(widgets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Stream, MAX_INLINE_DEPTH};

    fn dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut dict = Dictionary::new();
        for (key, value) in entries {
            dict.insert(*key, value.clone());
        }
        dict
    }

    // Minimal valid PDF; nodes are supplied via set_object refs (catalog
    // unused). Used by the `traverse_field`/`page_widget_annotation_refs`
    // unit tests below, which need arbitrary refs independent of any fixture
    // file's own object numbering.
    fn empty_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(std::io::Cursor::new(bytes)).expect("open")
    }

    fn refs_vec(nums: &[u32]) -> Vec<Object> {
        nums.iter()
            .map(|n| Object::Reference(ObjectRef::new(*n, 0)))
            .collect()
    }

    fn refs(nums: &[u32]) -> Object {
        Object::Array(refs_vec(nums))
    }

    /// Build `depth` levels of single-element arrays wrapping `Object::Null`.
    /// Contains no `Reference`, so walking it never reaches the resolve path.
    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    /// Minimal valid `Pdf` for tests that walk a `Reference`-free object. The
    /// `pdf` argument is required by the walker signature but is never touched
    /// because pure inline structure never reaches `pdf.resolve`.
    fn minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        Pdf::open_mem_owned(bytes.to_vec()).expect("open")
    }

    #[test]
    fn collect_refs_in_object_errors_on_excessive_inline_nesting() {
        let mut pdf = minimal_pdf();
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let deep = nested_arrays(MAX_INLINE_DEPTH + 5);
        // arg order: (pdf, obj, out, seen, depth, inline_depth, skip_parent_key)
        let err = collect_refs_in_object(&mut pdf, &deep, &mut out, &mut seen, 0, 0, true);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn collect_refs_in_object_accepts_inline_nesting_within_limit() {
        let mut pdf = minimal_pdf();
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        // Null leaf sits at inline_depth = MAX_INLINE_DEPTH, the deepest level
        // accepted under the strict `>` guard.
        let deep = nested_arrays(MAX_INLINE_DEPTH);
        collect_refs_in_object(&mut pdf, &deep, &mut out, &mut seen, 0, 0, true).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_refs_in_object_walks_dict_and_stream_arms_within_limit() {
        let mut pdf = minimal_pdf();
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        // Shallow object exercising the Dictionary and Stream arms (no Reference,
        // so the resolve path is never hit and `pdf` stays unused by the walk).
        let stream = Object::Stream(Stream::new(
            dict(&[("Length", Object::Integer(0))]),
            Vec::new(),
        ));
        let obj = Object::Dictionary(dict(&[("S", stream), ("N", Object::Integer(1))]));
        collect_refs_in_object(&mut pdf, &obj, &mut out, &mut seen, 0, 0, true).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn merge_acroform_dr_keeps_existing_when_source_is_not_a_dictionary() {
        let target_dr = Object::Dictionary(dict(&[(
            "Font",
            Object::Dictionary(dict(&[("Helv", Object::Integer(1))])),
        )]));
        let mut acroform = dict(&[("DR", target_dr.clone())]);

        let renames = merge_acroform_dr(&mut acroform, Object::Name(b"Bad".to_vec()));

        assert!(renames.is_empty());
        assert_eq!(acroform.get("DR"), Some(&target_dr));
    }

    #[test]
    fn merge_acroform_dr_preserves_non_dictionary_target() {
        let existing = Object::Name(b"Bad".to_vec());
        let mut acroform = dict(&[("DR", existing.clone())]);
        let source_dr = Object::Dictionary(dict(&[(
            "Font",
            Object::Dictionary(dict(&[("Helv", Object::Integer(1))])),
        )]));

        let renames = merge_acroform_dr(&mut acroform, source_dr);

        assert!(renames.is_empty());
        assert_eq!(acroform.get("DR"), Some(&existing));
    }

    #[test]
    fn merge_acroform_dr_inserts_source_when_target_is_missing_or_null() {
        for initial in [None, Some(Object::Null)] {
            let mut acroform = Dictionary::new();
            if let Some(value) = initial {
                acroform.insert("DR", value);
            }
            let source_dr = Object::Dictionary(dict(&[(
                "Font",
                Object::Dictionary(dict(&[("Helv", Object::Integer(1))])),
            )]));

            let renames = merge_acroform_dr(&mut acroform, source_dr.clone());

            assert!(renames.is_empty());
            assert_eq!(acroform.get("DR"), Some(&source_dr));
        }
    }

    #[test]
    fn merge_resource_dicts_keeps_target_non_dictionary_categories() {
        let target = dict(&[("Font", Object::Name(b"Existing".to_vec()))]);
        let source = dict(&[(
            "Font",
            Object::Dictionary(dict(&[("Helv", Object::Integer(1))])),
        )]);

        let (merged, renames) = merge_resource_dicts(target, source);

        assert!(renames.is_empty());
        assert_eq!(
            merged.get("Font"),
            Some(&Object::Name(b"Existing".to_vec()))
        );
    }

    #[test]
    fn merge_resource_category_skips_non_font_conflicts() {
        let target = dict(&[("Img", Object::Integer(1))]);
        let source = dict(&[("Img", Object::Integer(2))]);

        let (merged, renames) = merge_resource_category(target, &source, false);

        assert!(renames.is_empty());
        assert_eq!(merged.get("Img"), Some(&Object::Integer(1)));
    }

    #[test]
    fn unique_resource_name_uses_numeric_suffix_after_first_conflict() {
        let existing = dict(&[
            ("Helv_flpdf", Object::Integer(1)),
            ("Helv_flpdf2", Object::Integer(2)),
        ]);

        assert_eq!(unique_resource_name(b"Helv", &existing), b"Helv_flpdf3");
    }

    #[test]
    fn rewrite_da_resource_names_handles_non_strings_and_unmapped_names() {
        let mut renames = BTreeMap::new();
        renames.insert(b"Helv".to_vec(), b"Helv_flpdf".to_vec());

        assert_eq!(
            rewrite_da_resource_names(Object::Name(b"DA".to_vec()), &renames),
            Object::Name(b"DA".to_vec())
        );
        assert_eq!(
            rewrite_da_resource_names(
                Object::String(b"/Other 9 Tf /Helv2 10 Tf".to_vec()),
                &renames
            ),
            Object::String(b"/Other 9 Tf /Helv2 10 Tf".to_vec())
        );
    }

    // ── traverse_field / page_widget_annotation_refs (analyze() port) ──────

    #[test]
    fn traverse_field_visited_guard_breaks_kids_cycle() {
        // 5 -> /Kids [6] -> /Kids [5]: the loop guard must stop re-descending 5.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Kids", refs(&[6]))])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("Kids", refs(&[5]))])),
        );

        let mut visited = BTreeSet::new();
        let mut annotation_to_field = BTreeMap::new();
        traverse_field(
            &mut pdf,
            ObjectRef::new(5, 0),
            None,
            0,
            &mut visited,
            &mut annotation_to_field,
        )
        .unwrap();

        assert!(visited.contains(&ObjectRef::new(5, 0)));
        assert!(visited.contains(&ObjectRef::new(6, 0)));
        // Neither node is an annotation, so no field is recorded.
        assert!(annotation_to_field.is_empty());
    }

    #[test]
    fn traverse_field_ignores_non_dictionary_node() {
        // A non-dictionary field/annotation is skipped without visiting it.
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), Object::Integer(7));

        let mut visited = BTreeSet::new();
        let mut annotation_to_field = BTreeMap::new();
        traverse_field(
            &mut pdf,
            ObjectRef::new(5, 0),
            None,
            0,
            &mut visited,
            &mut annotation_to_field,
        )
        .unwrap();

        assert!(annotation_to_field.is_empty());
        assert!(!visited.contains(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn traverse_field_errs_past_depth_limit() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(dict(&[])));

        let mut visited = BTreeSet::new();
        let mut annotation_to_field = BTreeMap::new();
        let err = traverse_field(
            &mut pdf,
            ObjectRef::new(5, 0),
            None,
            DEFAULT_MAX_ACROFORM_DEPTH + 1,
            &mut visited,
            &mut annotation_to_field,
        );
        assert!(matches!(err, Err(Error::Unsupported(_))));
    }

    #[test]
    fn page_widget_annotation_refs_filters_widgets_and_edge_entries() {
        let mut pdf = empty_pdf();
        // page /Annots mixes: a widget, a non-widget, a non-dict, and a direct
        // (non-reference) entry — only the widget ref is returned.
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[(
                "Annots",
                Object::Array(vec![
                    Object::Reference(ObjectRef::new(5, 0)),
                    Object::Reference(ObjectRef::new(6, 0)),
                    Object::Reference(ObjectRef::new(7, 0)),
                    Object::Integer(99),
                ]),
            )])),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Widget".to_vec()))])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Link".to_vec()))])),
        );
        pdf.set_object(ObjectRef::new(7, 0), Object::Integer(0));

        let widgets = page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(widgets, vec![ObjectRef::new(5, 0)]);
    }

    #[test]
    fn page_widget_annotation_refs_resolves_an_indirect_subtype() {
        // Same qpdf `getKey("/Subtype")` transparent-dereference contract as
        // `get_field_for_annotation_resolves_an_indirect_subtype`.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[(
                "Annots",
                Object::Array(vec![Object::Reference(ObjectRef::new(5, 0))]),
            )])),
        );
        pdf.set_object(ObjectRef::new(9, 0), Object::Name(b"Widget".to_vec()));
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[(
                "Subtype",
                Object::Reference(ObjectRef::new(9, 0)),
            )])),
        );

        let widgets = page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0)).unwrap();
        assert_eq!(widgets, vec![ObjectRef::new(5, 0)]);
    }

    #[test]
    fn page_widget_annotation_refs_handles_non_dict_page_and_missing_annots() {
        let mut pdf = empty_pdf();
        // A non-dictionary page yields no widgets.
        pdf.set_object(ObjectRef::new(3, 0), Object::Integer(1));
        assert!(page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .is_empty());

        // A page dictionary without /Annots yields no widgets.
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[("Type", Object::Name(b"Page".to_vec()))])),
        );
        assert!(page_widget_annotation_refs(&mut pdf, ObjectRef::new(3, 0))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn annotation_to_field_map_merged_and_separated_widgets() {
        // AcroForm /Fields [7 8]; 7 is a merged field+widget (top-level,
        // /Subtype /Widget directly on it); 8 is a parent field with /Kids
        // [9], where 9 is a separated widget (/Parent 8, no /FT/T of its own).
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7, 8]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("FT", Object::Name(b"Tx".to_vec())),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[("Kids", refs(&[9]))])),
        );
        pdf.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(8, 0))),
            ])),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf);
        let map = helper.annotation_to_field_map().unwrap();
        assert_eq!(map.get(&ObjectRef::new(7, 0)), Some(&ObjectRef::new(7, 0)));
        // `annotation_to_field` maps a separated widget with its own
        // /Parent to itself, not its structural parent — qpdf's real
        // `traverseField`: `is_field` is true here because /Parent is
        // present, so `our_field = field` (9), matching
        // `libqpdf/QPDFAcroFormDocumentHelper.cc:343-347`. Callers that want
        // the named field walk the rest of the way with
        // `get_top_level_field` (see the next test).
        assert_eq!(map.get(&ObjectRef::new(9, 0)), Some(&ObjectRef::new(9, 0)));
        assert_eq!(
            helper.get_top_level_field(ObjectRef::new(9, 0)).unwrap(),
            ObjectRef::new(8, 0)
        );
        // A merged top-level field/widget has no /Parent: get_top_level_field
        // is a no-op.
        assert_eq!(
            helper.get_top_level_field(ObjectRef::new(7, 0)).unwrap(),
            ObjectRef::new(7, 0)
        );
    }

    #[test]
    fn get_top_level_field_stops_at_cycle() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(6, 0)))])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(5, 0)))])),
        );
        // Must terminate rather than looping forever.
        let top = AcroFormDocumentHelper::new(&mut pdf)
            .get_top_level_field(ObjectRef::new(5, 0))
            .unwrap();
        assert!(top == ObjectRef::new(5, 0) || top == ObjectRef::new(6, 0));
    }

    #[test]
    fn get_top_level_field_stops_at_non_dictionary_parent() {
        // 5's /Parent is an indirect reference to 6, but 6 doesn't resolve to
        // a dictionary. qpdf's getTopLevelField advances onto /Parent's
        // target unconditionally (`libqpdf/QPDFFormFieldObjectHelper.cc:34-45`
        // — no dictionary check before the `top_field = top_field.getKey(
        // "/Parent")` assignment) and only checks dict-ness at the top of the
        // *next* iteration (`getKeyIfDict`), so the walk stops on 6 itself
        // rather than backing up to 5.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(6, 0)))])),
        );
        pdf.set_object(ObjectRef::new(6, 0), Object::Integer(42));

        let top = AcroFormDocumentHelper::new(&mut pdf)
            .get_top_level_field(ObjectRef::new(5, 0))
            .unwrap();
        assert_eq!(top, ObjectRef::new(6, 0));
    }

    #[test]
    fn annotation_to_field_map_orphan_widget_self_maps() {
        // A widget on a page's /Annots that /AcroForm/Fields never reaches
        // becomes its own field (qpdf's orphan-widget fallback).
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", Object::Array(vec![]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[5])),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Widget".to_vec()))])),
        );

        let map = AcroFormDocumentHelper::new(&mut pdf)
            .annotation_to_field_map()
            .unwrap();
        assert_eq!(map.get(&ObjectRef::new(5, 0)), Some(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn annotation_to_field_map_treats_a_malformed_indirect_acroform_as_absent() {
        // qpdf's `analyze()` (`QPDFAcroFormDocumentHelper.cc:241-243`) reads
        // `/AcroForm` through `getKey`, which transparently dereferences,
        // then treats any non-dictionary result as absent with no error --
        // regardless of whether the source was a direct value or an
        // indirect reference to a dangling/null/non-dictionary target.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                ("AcroForm", Object::Reference(ObjectRef::new(9, 0))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(ObjectRef::new(9, 0), Object::Null);

        let map = AcroFormDocumentHelper::new(&mut pdf)
            .annotation_to_field_map()
            .expect("a malformed indirect /AcroForm must degrade to an empty map, not Err");
        assert!(map.is_empty());
    }

    #[test]
    fn get_field_for_annotation_non_widget_returns_none_without_traversal() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Link".to_vec()))])),
        );
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .get_field_for_annotation(ObjectRef::new(5, 0))
                .unwrap(),
            None
        );
    }

    #[test]
    fn get_field_for_annotation_missing_subtype_returns_none_without_traversal() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(dict(&[])));
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .get_field_for_annotation(ObjectRef::new(5, 0))
                .unwrap(),
            None
        );
    }

    #[test]
    fn get_field_for_annotation_looks_up_the_map() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("FT", Object::Name(b"Tx".to_vec())),
            ])),
        );
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .get_field_for_annotation(ObjectRef::new(7, 0))
                .unwrap(),
            Some(ObjectRef::new(7, 0))
        );
    }

    #[test]
    fn get_field_for_annotation_resolves_an_indirect_subtype() {
        // qpdf's `getFieldForAnnotation` gate
        // (`isDictionaryOfType("", "/Widget")`) transparently dereferences
        // `getKey("/Subtype")`, so a /Subtype stored as an indirect reference
        // must classify as a widget the same as a direct one.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(ObjectRef::new(9, 0), Object::Name(b"Widget".to_vec()));
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Reference(ObjectRef::new(9, 0))),
                ("FT", Object::Name(b"Tx".to_vec())),
            ])),
        );
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .get_field_for_annotation(ObjectRef::new(7, 0))
                .unwrap(),
            Some(ObjectRef::new(7, 0))
        );
    }
}
