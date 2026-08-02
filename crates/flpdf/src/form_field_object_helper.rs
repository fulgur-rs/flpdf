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

    /// Return the inheritable `/Ff` field flags.
    pub fn field_flags(&mut self) -> Result<Option<i64>> {
        self.resolve_inherited_integer(b"Ff")
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
