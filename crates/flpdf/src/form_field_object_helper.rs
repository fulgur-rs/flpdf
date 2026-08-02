//! qpdf correspondence: `QPDFFormFieldObjectHelper.cc`.
//!
//! Read-only access to AcroForm field dictionaries and their inheritable
//! attributes. This intentionally covers only the object-helper read boundary.

use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
use crate::{Error, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Typed read-only accessor helper for a PDF AcroForm field or widget
/// annotation dictionary.
pub struct FormFieldObjectHelper<'a, R: Read + Seek> {
    field_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FormFieldObjectHelper<'a, R> {
    /// Construct a new helper for the form field at `field_ref`.
    pub fn new(field_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { field_ref, pdf }
    }

    /// Return whether the referenced field object is PDF null.
    pub fn is_null(&mut self) -> Result<bool> {
        Ok(matches!(self.pdf.resolve(self.field_ref)?, Object::Null))
    }

    /// Return this field's direct `/Parent` reference, if present.
    pub fn parent(&mut self) -> Result<Option<ObjectRef>> {
        self.direct_parent(self.field_ref)
    }

    /// Return the top-level field and whether it differs from this field.
    pub fn top_level_field(&mut self) -> Result<(ObjectRef, bool)> {
        let mut seen = BTreeSet::new();
        let mut top = self.field_ref;
        let mut is_different = false;
        let mut depth = 0;

        while seen.insert(top) {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, top
                )));
            }
            let Some(parent) = self.direct_parent(top)? else {
                break;
            };
            top = parent;
            is_different = true;
            depth += 1;
        }
        Ok((top, is_different))
    }

    /// Return an inheritable field value, resolving a direct indirect value.
    pub fn inheritable_value(&mut self, key: &[u8]) -> Result<Option<Object>> {
        self.resolve_inherited_object(key)
    }

    /// Return an inheritable PDF string as qpdf-style UTF-8 text.
    pub fn inheritable_string(&mut self, key: &[u8]) -> Result<String> {
        Ok(match self.resolve_inherited_object(key)? {
            Some(Object::String(value)) => Self::utf8_string(&value),
            _ => String::new(),
        })
    }

    /// Return an inheritable PDF name with its leading slash.
    pub fn inheritable_name(&mut self, key: &[u8]) -> Result<Vec<u8>> {
        Ok(match self.resolve_inherited_object(key)? {
            Some(Object::Name(value)) => {
                let mut name = Vec::with_capacity(value.len() + 1);
                name.push(b'/');
                name.extend(value);
                name
            }
            _ => Vec::new(),
        })
    }

    /// Return the inheritable `/FT` field type as qpdf-style name bytes,
    /// including the leading slash.
    pub fn field_type(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.resolve_inherited_name(b"FT")?.map(|name| {
            let mut qpdf_name = Vec::with_capacity(name.len() + 1);
            qpdf_name.push(b'/');
            qpdf_name.extend(name);
            qpdf_name
        }))
    }

    /// Return the inheritable `/V` field value.
    pub fn field_value(&mut self) -> Result<Option<Object>> {
        self.resolve_inherited_object(b"V")
    }

    /// Return the inheritable `/DV` field default value.
    pub fn field_default_value(&mut self) -> Result<Option<Object>> {
        self.resolve_inherited_object(b"DV")
    }

    /// Return the inheritable `/V` field value.
    pub fn value(&mut self) -> Result<Option<Object>> {
        self.field_value()
    }

    /// Return the inheritable `/V` as qpdf-style UTF-8 text.
    pub fn value_as_string(&mut self) -> Result<String> {
        self.inheritable_string(b"V")
    }

    /// Return the inheritable `/DV` field default value.
    pub fn default_value(&mut self) -> Result<Option<Object>> {
        self.field_default_value()
    }

    /// Return the inheritable `/DV` as qpdf-style UTF-8 text.
    pub fn default_value_as_string(&mut self) -> Result<String> {
        self.inheritable_string(b"DV")
    }

    /// Return the inheritable `/Ff` field flags.
    pub fn field_flags(&mut self) -> Result<Option<i64>> {
        self.resolve_inherited_integer(b"Ff")
    }

    /// Return the inheritable `/Ff` flags, defaulting to zero.
    pub fn flags(&mut self) -> Result<i64> {
        Ok(self.field_flags()?.unwrap_or(0))
    }

    /// Return this field's `/T` partial name, if it is a string.
    pub fn partial_name(&mut self) -> Result<Option<Vec<u8>>> {
        self.string_key(self.field_ref, b"T")
    }

    /// Return the dotted `/T` name formed by this field and its parents.
    pub fn fully_qualified_name(&mut self) -> Result<Option<Vec<u8>>> {
        let mut seen = BTreeSet::new();
        let mut current = self.field_ref;
        let mut parts = Vec::new();

        while seen.insert(current) {
            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                break;
            };
            let name = dict.get("T").cloned();
            let parent = match dict.get("Parent") {
                Some(Object::Reference(parent)) => Some(*parent),
                _ => None,
            };
            let _ = node_obj;
            if let Some(name) = self.resolve_string(name)? {
                parts.push(name);
            }
            match parent {
                Some(parent) => current = parent,
                None => break,
            }
        }

        if parts.is_empty() {
            return Ok(None);
        }
        parts.reverse();
        let mut result = Vec::new();
        for (index, part) in parts.into_iter().enumerate() {
            if index != 0 {
                result.push(b'.');
            }
            result.extend(part);
        }
        Ok(Some(result))
    }

    /// Return `/TU`, or the fully qualified name when `/TU` is absent.
    pub fn alternative_name(&mut self) -> Result<Option<Vec<u8>>> {
        match self.string_key(self.field_ref, b"TU")? {
            Some(name) => Ok(Some(name)),
            None => self.fully_qualified_name(),
        }
    }

    /// Return `/TM`, then `/TU`, then the fully qualified name.
    pub fn mapping_name(&mut self) -> Result<Option<Vec<u8>>> {
        match self.string_key(self.field_ref, b"TM")? {
            Some(name) => Ok(Some(name)),
            None => self.alternative_name(),
        }
    }

    /// Return the default appearance string, inheriting `/DA` from the field
    /// tree and then falling back to `/AcroForm`.
    pub fn default_appearance(&mut self) -> Result<String> {
        let value = self.resolve_inherited_object(b"DA")?;
        let value = match value {
            Some(Object::String(value)) => return Ok(Self::utf8_string(&value)),
            _ => self.acroform_value(b"DA")?,
        };
        Ok(match value {
            Some(Object::String(value)) => Self::utf8_string(&value),
            _ => String::new(),
        })
    }

    /// Return the document-level `/AcroForm/DR` value.
    ///
    /// qpdf deliberately does not inherit `/DR` through the field tree.
    pub fn default_resources(&mut self) -> Result<Option<Object>> {
        self.acroform_value(b"DR")
    }

    /// Return the quadding value, inheriting `/Q` and then falling back to
    /// `/AcroForm/Q`. Missing or non-integer values are zero as in qpdf.
    pub fn quadding(&mut self) -> Result<i64> {
        let value = self.resolve_inherited_object(b"Q")?;
        match value {
            Some(Object::Integer(value)) => Ok(value),
            _ => Ok(match self.acroform_value(b"Q")? {
                Some(Object::Integer(value)) => value,
                _ => 0,
            }),
        }
    }

    /// Return whether this field is a text field (`/FT /Tx`).
    pub fn is_text(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Tx"))
    }

    /// Return whether this field is a checkbox button.
    ///
    /// qpdf defines a checkbox as a `/Btn` field with neither the radio nor
    /// pushbutton flag set.
    pub fn is_checkbox(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Btn")
            && self.field_flags()?.unwrap_or(0) & ((1 << 15) | (1 << 16)) == 0)
    }

    /// Return whether this checkbox has an on-state value.
    ///
    /// qpdf's post-11.9 implementation defines this as a checkbox whose
    /// inheritable `/V` is a name other than `/Off`.
    pub fn is_checked(&mut self) -> Result<bool> {
        Ok(self.is_checkbox()?
            && matches!(self.field_value()?, Some(Object::Name(value)) if value != b"Off"))
    }

    /// Return whether this field is a radio button (`/Btn`, flag bit 16).
    pub fn is_radio_button(&mut self) -> Result<bool> {
        const RADIO: i64 = 1 << 15;
        Ok(self.field_type()?.as_deref() == Some(b"/Btn")
            && self.field_flags()?.unwrap_or(0) & RADIO == RADIO)
    }

    /// Return whether this field is a pushbutton (`/Btn`, flag bit 17).
    pub fn is_pushbutton(&mut self) -> Result<bool> {
        const PUSHBUTTON: i64 = 1 << 16;
        Ok(self.field_type()?.as_deref() == Some(b"/Btn")
            && self.field_flags()?.unwrap_or(0) & PUSHBUTTON == PUSHBUTTON)
    }

    /// Return whether this field is a choice field (`/FT /Ch`).
    pub fn is_choice(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Ch"))
    }

    /// Return qpdf's choice labels for a choice field.
    ///
    /// This mirrors qpdf 11.9.0 `getChoices()`: only string items in the
    /// inheritable `/Opt` array are returned. PDF export/display pairs and
    /// other malformed items are ignored.
    pub fn choices(&mut self) -> Result<Vec<String>> {
        if !self.is_choice()? {
            return Ok(Vec::new());
        }

        let Some(options) = self.resolve_inherited_object(b"Opt")? else {
            return Ok(Vec::new());
        };
        let Some(items) = options.as_array() else {
            return Ok(Vec::new());
        };

        let mut choices = Vec::new();
        for item in items {
            let item = match item {
                Object::Reference(reference) => self.pdf.resolve(*reference)?,
                item => item.clone(),
            };
            if let Object::String(value) = item {
                choices.push(Self::utf8_string(&value));
            }
        }
        Ok(choices)
    }

    fn acroform_value(&mut self, key: &[u8]) -> Result<Option<Object>> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let root = self.pdf.resolve(root_ref)?;
        let Some(acroform) = root
            .as_dict()
            .and_then(|dict| dict.get("AcroForm"))
            .cloned()
        else {
            return Ok(None);
        };
        let acroform = match acroform {
            Object::Reference(reference) => self.pdf.resolve(reference)?,
            value => value,
        };
        let Some(value) = acroform.as_dict().and_then(|dict| dict.get(key)).cloned() else {
            return Ok(None);
        };
        let value = match value {
            Object::Reference(reference) => self.pdf.resolve(reference)?,
            value => value,
        };
        Ok((!matches!(value, Object::Null)).then_some(value))
    }

    fn utf8_string(value: &[u8]) -> String {
        let value = crate::pdf_string::utf8_value(value);
        String::from_utf8_lossy(&value).into_owned()
    }

    fn direct_parent(&mut self, field_ref: ObjectRef) -> Result<Option<ObjectRef>> {
        let node = self.pdf.resolve_borrowed(field_ref)?;
        Ok(match node.as_dict().and_then(|dict| dict.get("Parent")) {
            Some(Object::Reference(parent)) => Some(*parent),
            _ => None,
        })
    }

    fn resolve_inherited_name(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut seen = BTreeSet::new();
        let mut current = self.field_ref;
        let mut depth = 0;
        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, current
                )));
            }
            if !seen.insert(current) {
                return Ok(None);
            }
            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Ok(None);
            };
            let found = dict.get(key).cloned();
            let parent_ref = match dict.get("Parent") {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            };
            let _ = node_obj;
            if let Some(value) = found {
                let resolved = match value {
                    Object::Reference(r) => self.pdf.resolve(r)?,
                    other => other,
                };
                if let Object::Name(bytes) = resolved {
                    return Ok(Some(bytes));
                }
                if !matches!(resolved, Object::Null) {
                    return Ok(None);
                }
            }
            match parent_ref {
                Some(parent) => {
                    current = parent;
                    depth += 1;
                }
                None => return Ok(None),
            }
        }
    }

    fn resolve_inherited_object(&mut self, key: &[u8]) -> Result<Option<Object>> {
        let mut seen = BTreeSet::new();
        let mut current = self.field_ref;
        let mut depth = 0;
        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, current
                )));
            }
            if !seen.insert(current) {
                return Ok(None);
            }
            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Ok(None);
            };
            let found = dict.get(key).cloned();
            let parent_ref = match dict.get("Parent") {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            };
            let _ = node_obj;
            if let Some(value) = found {
                match value {
                    Object::Null => {}
                    Object::Reference(r) => match self.pdf.resolve(r)? {
                        Object::Null => {}
                        resolved => return Ok(Some(resolved)),
                    },
                    other => return Ok(Some(other)),
                }
            }
            match parent_ref {
                Some(parent) => {
                    current = parent;
                    depth += 1;
                }
                None => return Ok(None),
            }
        }
    }

    fn resolve_inherited_integer(&mut self, key: &[u8]) -> Result<Option<i64>> {
        let mut seen = BTreeSet::new();
        let mut current = self.field_ref;
        let mut depth = 0;
        loop {
            if depth >= DEFAULT_MAX_PAGE_TREE_DEPTH {
                return Err(Error::Unsupported(format!(
                    "field tree depth exceeds maximum of {} at {}",
                    DEFAULT_MAX_PAGE_TREE_DEPTH, current
                )));
            }
            if !seen.insert(current) {
                return Ok(None);
            }
            let node_obj = self.pdf.resolve_borrowed(current)?;
            let Some(dict) = node_obj.as_dict() else {
                return Ok(None);
            };
            let found = dict.get(key).cloned();
            let parent_ref = match dict.get("Parent") {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            };
            let _ = node_obj;
            if let Some(value) = found {
                let resolved = match value {
                    Object::Reference(r) => self.pdf.resolve(r)?,
                    other => other,
                };
                match resolved {
                    Object::Null => {}
                    Object::Integer(value) => return Ok(Some(value)),
                    _ => return Ok(Some(0)),
                }
            }
            match parent_ref {
                Some(parent) => {
                    current = parent;
                    depth += 1;
                }
                None => return Ok(None),
            }
        }
    }

    fn string_key(&mut self, field_ref: ObjectRef, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let node_obj = self.pdf.resolve_borrowed(field_ref)?;
        let Some(dict) = node_obj.as_dict() else {
            return Ok(None);
        };
        let value = dict.get(key).cloned();
        let _ = node_obj;
        self.resolve_string(value)
    }

    fn resolve_string(&mut self, value: Option<Object>) -> Result<Option<Vec<u8>>> {
        let value = match value {
            Some(Object::Reference(reference)) => self.pdf.resolve(reference)?,
            Some(value) => value,
            None => return Ok(None),
        };
        Ok(match value {
            Object::String(value) => Some(value),
            _ => None,
        })
    }
}
