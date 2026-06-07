//! High-level AcroForm document helper.
//!
//! [`AcroFormDocumentHelper`] wraps a `&mut Pdf<R>` and exposes document-level
//! operations for interactive form fields. It builds on
//! [`crate::FormFieldObjectHelper`] for inherited value lookup and on
//! [`crate::copy_objects`] for cross-document field copying.

use crate::{
    copy_objects, json_inspect::decode_pdf_text_string, Dictionary, Error, FormFieldObjectHelper,
    Object, ObjectRef, Pdf, Result, DEFAULT_MAX_ACROFORM_DEPTH,
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
pub struct AcroFormDocumentHelper<'a, R: Read + Seek> {
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

    /// Return the field's inherited `/V` value.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when a field-tree node is not a dictionary, or
    ///   when the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn field_value(&mut self, field_ref: ObjectRef) -> Result<Option<Object>> {
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
    pub fn set_field_value(&mut self, field_ref: ObjectRef, value: Object) -> Result<()> {
        let mut dict = self.resolve_field_dict(field_ref)?;
        dict.insert("V", value);
        self.pdf.set_object(field_ref, Object::Dictionary(dict));
        Ok(())
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
            Some(Object::Reference(acroform_ref)) => {
                Ok(Some(self.resolve_dict(acroform_ref, "AcroForm")?))
            }
            Some(_) => Ok(None),
        }
    }

    fn ensure_acroform_ref(&mut self) -> Result<ObjectRef> {
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
        collect_reachable_refs(helper.pdf, *field_ref, &mut copy_set, &mut seen, 0)?;
    }
    for (_, value) in &inherited_entries {
        collect_refs_in_object(helper.pdf, value, &mut copy_set, &mut seen, 0)?;
    }
    Ok((top_fields, inherited_entries, copy_set))
}

impl<'a, R: Read + Seek> AcroFormDocumentHelper<'a, R> {
    fn top_level_fields(&mut self) -> Result<Vec<ObjectRef>> {
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

    fn acroform_inherited_entries(&mut self) -> Result<Vec<(Vec<u8>, Object)>> {
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

fn collect_reachable_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object_ref: ObjectRef,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    // The `seen` cycle guard cannot stop a long *acyclic* indirect-reference chain
    // (obj1 -> obj2 -> ... -> objN), where recursion depth grows with the chain length.
    // Bound the reference chain to avoid stack overflow on hostile source PDFs. Only the
    // indirect-reference axis is unbounded; intra-object nesting is already capped by the
    // parser, so `depth` increments per resolved reference (see `collect_refs_in_object`).
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
    collect_refs_in_object(pdf, &obj, out, seen, depth)
}

fn collect_refs_in_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    match obj {
        Object::Reference(object_ref) => {
            collect_reachable_refs(pdf, *object_ref, out, seen, depth + 1)
        }
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(pdf, item, out, seen, depth)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => collect_refs_in_dict(pdf, dict, out, seen, depth),
        Object::Stream(stream) => collect_refs_in_dict(pdf, &stream.dict, out, seen, depth),
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => Ok(()),
    }
}

fn collect_refs_in_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    for (key, value) in dict.iter() {
        if key == b"P" {
            continue;
        }
        collect_refs_in_object(pdf, value, out, seen, depth)?;
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
        Some(Object::Reference(object_ref)) => match pdf.resolve_borrowed(object_ref)? {
            Object::Array(values) => Ok(Some(values.clone())),
            Object::Null => Ok(None),
            _ => Ok(None),
        },
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

fn remap_refs_in_object(obj: Object, map: &BTreeMap<ObjectRef, ObjectRef>) -> Object {
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
        | Object::Name(_)
        | Object::String(_) => obj,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut dict = Dictionary::new();
        for (key, value) in entries {
            dict.insert(*key, value.clone());
        }
        dict
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
}
