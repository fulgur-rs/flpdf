//! High-level AcroForm document helper.
//!
//! [`AcroFormDocumentHelper`] wraps a `&mut Pdf<R>` and exposes document-level
//! operations for interactive form fields. It builds on
//! [`crate::FormFieldObjectHelper`] for inherited value lookup and on
//! [`crate::copy_objects`] for cross-document field copying.

use crate::{
    copy_objects, Dictionary, Error, FormFieldObjectHelper, Object, ObjectRef, Pdf, Result,
};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

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

    /// Return the field's inherited `/V` value.
    pub fn field_value(&mut self, field_ref: ObjectRef) -> Result<Option<Object>> {
        FormFieldObjectHelper::new(field_ref, self.pdf).field_value()
    }

    /// Set the field's direct `/V` value.
    ///
    /// This updates the field dictionary itself. It does not synthesize widget
    /// appearance streams.
    pub fn set_field_value(&mut self, field_ref: ObjectRef, value: Object) -> Result<()> {
        let mut dict = self.resolve_field_dict(field_ref)?;
        dict.insert("V", value);
        self.pdf.set_object(field_ref, Object::Dictionary(dict));
        Ok(())
    }

    /// Set `/AcroForm/DA`, creating `/AcroForm` if needed.
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
    pub fn fix_appearance_inheritance(&mut self) -> Result<()> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(());
        };
        let Some(da) = acroform.get("DA").cloned() else {
            return Ok(());
        };

        for field_ref in self.fields()? {
            let mut field = self.resolve_field_dict(field_ref)?;
            if field.get("DA").is_none() {
                field.insert("DA", da.clone());
                self.pdf.set_object(field_ref, Object::Dictionary(field));
            }
        }
        Ok(())
    }

    /// Copy all top-level fields from `source` and append them to this document.
    ///
    /// Returns the copied top-level field refs in the target document.
    pub fn copy_fields_from<RS: Read + Seek>(
        &mut self,
        source: &mut Pdf<RS>,
    ) -> Result<Vec<ObjectRef>> {
        let (top_fields, copy_set) = source_field_copy_set(source)?;
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
        let mut fields = match resolve_array_value(self.pdf, acroform.get("Fields").cloned())? {
            Some(values) => values,
            None => Vec::new(),
        };
        fields.extend(copied_top.iter().copied().map(Object::Reference));
        acroform.insert("Fields", Object::Array(fields));
        self.pdf
            .set_object(acroform_ref, Object::Dictionary(acroform));

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
                self.walk_field_tree(kid_ref, seen, out)?;
            }
        }
        Ok(())
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level AcroForm helper for this document.
    pub fn acroform(&mut self) -> AcroFormDocumentHelper<'_, R> {
        AcroFormDocumentHelper::new(self)
    }
}

fn source_field_copy_set<RS: Read + Seek>(
    source: &mut Pdf<RS>,
) -> Result<(Vec<ObjectRef>, BTreeSet<ObjectRef>)> {
    let mut helper = AcroFormDocumentHelper::new(source);
    let top_fields = helper.top_level_fields()?;
    let mut copy_set = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for field_ref in &top_fields {
        collect_reachable_refs(helper.pdf, *field_ref, &mut copy_set, &mut seen)?;
    }
    Ok((top_fields, copy_set))
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
}

fn collect_reachable_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object_ref: ObjectRef,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    if !seen.insert(object_ref) {
        return Ok(());
    }
    out.insert(object_ref);

    let obj = pdf.resolve(object_ref)?;
    collect_refs_in_object(pdf, &obj, out, seen)
}

fn collect_refs_in_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    match obj {
        Object::Reference(object_ref) => collect_reachable_refs(pdf, *object_ref, out, seen),
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(pdf, item, out, seen)?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => collect_refs_in_dict(pdf, dict, out, seen),
        Object::Stream(stream) => collect_refs_in_dict(pdf, &stream.dict, out, seen),
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
) -> Result<()> {
    for (key, value) in dict.iter() {
        if key == b"P" {
            continue;
        }
        collect_refs_in_object(pdf, value, out, seen)?;
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
