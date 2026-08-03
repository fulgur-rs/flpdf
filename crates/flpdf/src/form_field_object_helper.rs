//! qpdf correspondence: `QPDFFormFieldObjectHelper.cc`.
//!
//! Read-only access to AcroForm field dictionaries and their inheritable
//! attributes. This intentionally covers only the object-helper read boundary.

use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

#[path = "form_field_object_helper/rendering.rs"]
mod rendering;

/// Typed read-only accessor helper for a PDF AcroForm field or widget
/// annotation dictionary.
pub struct FormFieldObjectHelper<'a, R: Read + Seek + 'static> {
    field_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek + 'static> FormFieldObjectHelper<'a, R> {
    /// Construct a new helper for the form field at `field_ref`.
    pub fn new(field_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { field_ref, pdf }
    }

    /// Return whether the referenced field object is PDF null.
    pub fn is_null(&mut self) -> Result<bool> {
        let field = self.pdf.get_object_handle(self.field_ref);
        let field = self.pdf.resolve_object_handle_to_terminal(&field)?;
        Ok(field.is_null())
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

    /// Return the inheritable `/V` handle without discarding indirect identity.
    ///
    /// Typed consumers should use [`Self::field_value`]. JSON and signature
    /// inspection use this handle-aware boundary because qpdf's `getValue()`
    /// keeps the selected `QPDFObjectHandle` indirect while its typed accessors
    /// transparently dereference the terminal value.
    pub fn field_value_handle(&mut self) -> Result<Option<ObjectHandle>> {
        self.resolve_inherited_handle(b"V")
    }

    /// Return the indirect object reference used by the inherited `/V`, when
    /// the selected value is indirect. This preserves the signature
    /// inspector's observable reference reporting without giving consumers a
    /// second parent-chain walker.
    pub fn field_value_reference(&mut self) -> Result<Option<ObjectRef>> {
        Ok(self
            .field_value_handle()?
            .and_then(|value| value.object_ref().or_else(|| value.as_reference())))
    }

    /// Return the inheritable `/DV` field default value.
    pub fn field_default_value(&mut self) -> Result<Option<Object>> {
        self.resolve_inherited_object(b"DV")
    }

    /// Return the inheritable `/DV` handle without discarding indirect identity.
    pub fn field_default_value_handle(&mut self) -> Result<Option<ObjectHandle>> {
        self.resolve_inherited_handle(b"DV")
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

    /// Return this field's `/T` partial name as qpdf-style UTF-8 text.
    pub fn partial_name(&mut self) -> Result<String> {
        Ok(self.string_key(self.field_ref, b"T")?.unwrap_or_default())
    }

    /// Return the dotted `/T` name formed by this field and its parents.
    pub fn fully_qualified_name(&mut self) -> Result<String> {
        let mut seen = BTreeSet::new();
        let mut current = self.field_ref;
        let mut parts = Vec::new();

        while seen.insert(current) {
            let node = self.pdf.get_object_handle(current);
            let node = self.pdf.resolve_object_handle_to_terminal(&node)?;
            let Some(dict) = node.as_dictionary() else {
                break;
            };
            let name = dict.get(b"T".as_slice()).cloned();
            let parent = dict
                .get(b"Parent".as_slice())
                .and_then(|parent| parent.object_ref().or_else(|| parent.as_reference()));
            if let Some(name) = self.resolve_string_handle(name)? {
                parts.push(name);
            }
            match parent {
                Some(parent) => current = parent,
                None => break,
            }
        }

        parts.reverse();
        Ok(parts.join("."))
    }

    /// Return `/TU`, or the fully qualified name when `/TU` is absent.
    pub fn alternative_name(&mut self) -> Result<String> {
        match self.string_key(self.field_ref, b"TU")? {
            Some(name) => Ok(name),
            None => self.fully_qualified_name(),
        }
    }

    /// Return `/TM`, then `/TU`, then the fully qualified name.
    pub fn mapping_name(&mut self) -> Result<String> {
        match self.string_key(self.field_ref, b"TM")? {
            Some(name) => Ok(name),
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

        let Some(options) = self.resolve_inherited_handle(b"Opt")? else {
            return Ok(Vec::new());
        };
        let options = self.pdf.resolve_object_handle_to_terminal(&options)?;
        let Some(items) = options.as_array() else {
            return Ok(Vec::new());
        };

        let mut choices = Vec::new();
        for item in items {
            let item = self.pdf.resolve_object_handle_to_terminal(&item)?;
            if let Some(value) = item.as_string() {
                choices.push(Self::utf8_string(&value));
            }
        }
        Ok(choices)
    }

    /// Replace a direct attribute on this field dictionary.
    ///
    /// This is qpdf's `setFieldAttribute(key, value)`: inherited attributes
    /// are never modified on an ancestor.
    pub fn set_field_attribute(&mut self, key: &[u8], value: Object) -> Result<()> {
        self.set_direct_attribute(self.field_ref, key, value)
    }

    /// Replace a direct string attribute using qpdf's Unicode-string encoding.
    pub fn set_field_attribute_string(&mut self, key: &[u8], value: &str) -> Result<()> {
        self.set_field_attribute(
            key,
            Object::String(crate::pdf_string::new_unicode_string(value.as_bytes())),
        )
    }

    /// Set the field's `/V` value using qpdf's form-field dispatch.
    ///
    /// Text and choice strings are normalized to a PDF Unicode string.  A
    /// requested `/NeedAppearances` update applies only to non-button fields;
    /// button fields instead synchronize their widget appearance states.
    pub fn set_value(&mut self, value: Object, need_appearances: bool) -> Result<()> {
        if self.field_type()?.as_deref() == Some(b"/Btn") {
            if self.is_checkbox()? {
                if let Object::Name(name) = value {
                    self.set_checkbox_value(name != b"Off")?;
                }
            } else if self.is_radio_button()? {
                if let Object::Name(name) = value {
                    self.set_radio_button_value(self.field_ref, name)?;
                } // cov:ignore: llvm-cov attributes the covered radio dispatch edge to the closing brace
            }
            // qpdf intentionally ignores both invalid button input and
            // pushbutton values after issuing a warning when available.
            return Ok(());
        }

        let value = match value {
            Object::String(value) => Object::String(crate::pdf_string::new_unicode_string(
                &crate::pdf_string::utf8_value(&value),
            )),
            value => value,
        };
        self.set_field_attribute(b"V", value)?;
        if need_appearances {
            self.set_need_appearances()?;
        } // cov:ignore: llvm-cov attributes the covered NeedAppearances branch edge to the closing brace
        Ok(())
    }

    /// Set `/V` from UTF-8 text using qpdf's Unicode-string encoding.
    pub fn set_value_string(&mut self, value: &str, need_appearances: bool) -> Result<()> {
        self.set_value(
            Object::String(crate::pdf_string::new_unicode_string(value.as_bytes())),
            need_appearances,
        )
    }

    /// Generate a normal appearance for `widget_ref` from this field's value.
    ///
    /// Button appearance generation remains deliberately outside qpdf's
    /// `QPDFFormFieldObjectHelper::generateAppearance` dispatch.
    pub fn generate_appearance_for(&mut self, widget_ref: ObjectRef) -> Result<Option<ObjectRef>> {
        match self.field_type()?.as_deref() {
            Some(b"/Tx") => rendering::render_text_field(self.pdf, self.field_ref, widget_ref),
            Some(b"/Ch") => rendering::render_choice_field(self.pdf, self.field_ref, widget_ref),
            _ => Ok(None),
        }
    }

    /// Clear `/AcroForm/NeedAppearances` after a document appearance pass.
    ///
    /// As in qpdf, absent, indirect-non-dictionary, and non-true values are
    /// left untouched. The operation lives with the form-field helper so
    /// callers do not duplicate AcroForm ownership traversal.
    pub fn clear_need_appearances_after_generation(pdf: &mut Pdf<R>) -> Result<()> {
        let Some(root_ref) = pdf.root_ref() else {
            return Ok(());
        };
        let Object::Dictionary(mut root) = pdf.resolve(root_ref)? else {
            return Ok(());
        };
        let Some(acroform) = root.get("AcroForm").cloned() else {
            return Ok(());
        };
        let (acroform, terminal_ref) = resolve_ref_chain(pdf, &acroform)?;
        let Object::Dictionary(mut acroform) = acroform else {
            return Ok(());
        };
        let Some(need_appearances) = acroform.get("NeedAppearances").cloned() else {
            return Ok(());
        };
        let (need_appearances, _) = resolve_ref_chain(pdf, &need_appearances)?;
        if !matches!(need_appearances, Object::Boolean(true)) {
            return Ok(());
        }
        acroform.remove("NeedAppearances");
        if let Some(acroform_ref) = terminal_ref {
            pdf.set_object(acroform_ref, Object::Dictionary(acroform));
        } else {
            root.insert("AcroForm", Object::Dictionary(acroform));
            pdf.set_object(root_ref, Object::Dictionary(root));
        }
        Ok(())
    }

    fn field_dict(&mut self) -> Result<Dictionary> {
        self.field_dict_for(self.field_ref, "form field")
    }

    fn set_need_appearances(&mut self) -> Result<()> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(());
        };
        let root = self.pdf.resolve(root_ref)?;
        let Some(acroform) = root
            .as_dict()
            .and_then(|dictionary| dictionary.get(b"AcroForm".as_slice()))
            .cloned()
        else {
            return Ok(());
        };
        let (acroform, terminal_ref) = resolve_ref_chain(self.pdf, &acroform)?;
        let Some(mut acroform) = acroform.as_dict().cloned() else {
            return Ok(());
        };
        acroform.insert("NeedAppearances", Object::Boolean(true));
        if let Some(reference) = terminal_ref {
            self.pdf.set_object(reference, Object::Dictionary(acroform));
        } else {
            let mut root = self.field_dict_for(root_ref, "catalog")?;
            root.insert("AcroForm", Object::Dictionary(acroform));
            self.pdf.set_object(root_ref, Object::Dictionary(root));
        }
        Ok(())
    }

    fn set_checkbox_value(&mut self, checked: bool) -> Result<()> {
        let annotation = self.appearance_annotation(self.field_ref)?;
        if annotation.is_none() {
            if let Some((kids, state)) = self.update_direct_checkbox_kid(checked)? {
                self.set_field_attribute(b"V", Object::Name(state))?;
                self.set_direct_attribute(self.field_ref, b"Kids", kids)?;
                return Ok(());
            }
        }
        let on_value = self.checkbox_state(annotation.as_ref().map(|(_, dict)| dict), checked)?;
        let value = Object::Name(on_value);
        self.set_field_attribute(b"V", value.clone())?;
        if let Some((annotation_ref, _)) = annotation {
            self.set_direct_attribute(annotation_ref, b"AS", value)?;
        }
        Ok(())
    }

    /// qpdf permits direct widget dictionaries in `/Kids`; mutate the first
    /// such child with a usable appearance and preserve the containing array.
    fn update_direct_checkbox_kid(&mut self, checked: bool) -> Result<Option<(Object, Vec<u8>)>> {
        let field = self.field_dict()?;
        let Some(kids) = field.get(b"Kids".as_slice()).cloned() else {
            return Ok(None);
        };
        self.update_direct_checkbox_kid_in_array(kids, checked)
    }

    fn update_direct_checkbox_kid_in_array(
        &mut self,
        kids: Object,
        checked: bool,
    ) -> Result<Option<(Object, Vec<u8>)>> {
        let original_reference = match kids {
            Object::Reference(reference) => Some(reference),
            _ => None,
        };
        let Some((mut kids, terminal_ref)) = self.resolve_array_target(kids)? else {
            return Ok(None);
        };
        for kid in &mut kids {
            let Object::Dictionary(mut widget) = kid.clone() else {
                continue;
            };
            if !self.has_non_null_appearance(&widget)? {
                continue;
            }
            let state = self.checkbox_state(Some(&widget), checked)?;
            widget.insert("AS", Object::Name(state.clone()));
            *kid = Object::Dictionary(widget);
            let updated = Object::Array(kids);
            if let Some(reference) = terminal_ref {
                self.pdf.set_object(reference, updated);
                return Ok(original_reference
                    .map(|reference| (Object::Reference(reference), state.clone())));
            }
            return Ok(Some((updated, state)));
        }
        Ok(None)
    }

    fn checkbox_state(
        &mut self,
        annotation: Option<&Dictionary>,
        checked: bool,
    ) -> Result<Vec<u8>> {
        if !checked {
            return Ok(b"Off".to_vec());
        }
        Ok(annotation
            .map(|dictionary| self.normal_appearance_names(dictionary))
            .transpose()?
            .and_then(|names| names.into_iter().find(|name| name != b"Off"))
            .unwrap_or_else(|| b"Yes".to_vec()))
    }

    fn set_radio_button_value(&mut self, field_ref: ObjectRef, value: Vec<u8>) -> Result<()> {
        let field = self.field_dict_for(field_ref, "form field")?;
        if let Some(parent) = field.get_ref(b"Parent".as_slice()) {
            let parent_parent = self
                .dictionary_handle_for(parent)?
                .and_then(|parent| parent.as_dictionary())
                .map(|parent| {
                    parent
                        .get(b"Parent".as_slice())
                        .cloned()
                        .map(|parent| parent.materialize())
                });
            if let Some(parent_parent) = parent_parent {
                let parent_is_radio = if self.value_is_null(parent_parent)? {
                    let previous = self.field_ref;
                    self.field_ref = parent;
                    let result = self.is_radio_button();
                    self.field_ref = previous;
                    result?
                } else {
                    false
                };
                if parent_is_radio {
                    return self.set_radio_button_value(parent, value);
                }
            }
        }

        let parent_is_null = self.value_is_null(field.get(b"Parent".as_slice()).cloned())?;
        let Some(kids_value) = field.get(b"Kids".as_slice()).cloned() else {
            return Ok(());
        };
        if self.object_array(Some(kids_value.clone()))?.is_none() {
            return Ok(());
        }
        if !parent_is_null {
            return Ok(());
        }
        self.set_direct_attribute(field_ref, b"V", Object::Name(value.clone()))?;
        self.update_radio_kids(field_ref, kids_value, &value)?;
        Ok(())
    }

    fn update_radio_kids(
        &mut self,
        field_ref: ObjectRef,
        kids_value: Object,
        value: &[u8],
    ) -> Result<()> {
        let Some((kids, terminal_ref)) = self.resolve_array_target(kids_value)? else {
            return Ok(()); // cov:ignore: object_array above already accepted the terminal /Kids array
        };
        let mut updated = Vec::with_capacity(kids.len());
        for kid in kids {
            updated.push(self.update_radio_kid(kid, value)?);
        }
        let updated = Object::Array(updated);
        if let Some(reference) = terminal_ref {
            self.pdf.set_object(reference, updated);
        } else {
            self.set_direct_attribute(field_ref, b"Kids", updated)?;
        }
        Ok(())
    }

    /// qpdf looks one level below a radio child that has no `/AP`; it does
    /// not recurse beyond that child field.
    fn update_radio_kid(&mut self, kid: Object, value: &[u8]) -> Result<Object> {
        match kid {
            Object::Reference(reference) => {
                let Some(dictionary) = self.dictionary_handle_for(reference)? else {
                    return Ok(Object::Reference(reference));
                };
                let terminal_ref = dictionary.object_ref().unwrap_or(reference);
                let dictionary = dictionary
                    .materialize()
                    .into_dict()
                    .expect("dictionary handle must materialize as a dictionary");
                let dictionary = self.update_radio_kid_dict(dictionary, value)?;
                self.pdf
                    .set_object(terminal_ref, Object::Dictionary(dictionary));
                Ok(Object::Reference(reference))
            }
            Object::Dictionary(dictionary) => Ok(Object::Dictionary(
                self.update_radio_kid_dict(dictionary, value)?,
            )),
            object => Ok(object),
        }
    }

    fn update_radio_kid_dict(&mut self, mut kid: Dictionary, value: &[u8]) -> Result<Dictionary> {
        if self.has_non_null_appearance(&kid)? {
            kid.insert("AS", Object::Name(self.radio_state(&kid, value)?));
            return Ok(kid);
        }
        let Some(grandkids) = kid.get(b"Kids".as_slice()).cloned() else {
            return Ok(kid);
        };
        let (grandkids, _) = self.update_first_radio_widget(grandkids, value)?;
        kid.insert("Kids", grandkids);
        Ok(kid)
    }

    fn update_first_radio_widget(&mut self, kids: Object, value: &[u8]) -> Result<(Object, bool)> {
        let original_reference = kids.as_ref_id();
        let Some((mut kids, terminal_ref)) = self.resolve_array_target(kids.clone())? else {
            return Ok((kids, false));
        };
        let mut found = false;
        for kid in &mut kids {
            let (updated, updated_one) = self.update_radio_widget(kid.clone(), value)?;
            *kid = updated;
            if updated_one {
                found = true;
                break;
            }
        }
        let updated = Object::Array(kids);
        if let Some(reference) = terminal_ref {
            self.pdf.set_object(reference, updated);
            Ok((
                Object::Reference(original_reference.unwrap_or(reference)),
                found,
            ))
        } else {
            Ok((updated, found))
        }
    }

    fn update_radio_widget(&mut self, widget: Object, value: &[u8]) -> Result<(Object, bool)> {
        match widget {
            Object::Reference(reference) => {
                let Some(dictionary) = self.dictionary_handle_for(reference)? else {
                    return Ok((Object::Reference(reference), false));
                };
                let terminal_ref = dictionary.object_ref().unwrap_or(reference);
                let mut dictionary = dictionary
                    .materialize()
                    .into_dict()
                    .expect("dictionary handle must materialize as a dictionary");
                if !self.has_non_null_appearance(&dictionary)? {
                    return Ok((Object::Reference(reference), false));
                }
                dictionary.insert("AS", Object::Name(self.radio_state(&dictionary, value)?));
                self.pdf
                    .set_object(terminal_ref, Object::Dictionary(dictionary));
                Ok((Object::Reference(reference), true))
            }
            Object::Dictionary(mut dictionary) => {
                if !self.has_non_null_appearance(&dictionary)? {
                    return Ok((Object::Dictionary(dictionary), false));
                }
                dictionary.insert("AS", Object::Name(self.radio_state(&dictionary, value)?));
                Ok((Object::Dictionary(dictionary), true))
            }
            object => Ok((object, false)),
        }
    }

    fn radio_state(&mut self, widget: &Dictionary, value: &[u8]) -> Result<Vec<u8>> {
        let names = self.normal_appearance_names(widget)?;
        Ok(if names.iter().any(|name| name == value) {
            value.to_vec()
        } else {
            b"Off".to_vec()
        })
    }

    fn appearance_annotation(
        &mut self,
        start: ObjectRef,
    ) -> Result<Option<(ObjectRef, Dictionary)>> {
        let field = self.field_dict_for(start, "form field")?;
        if self.has_non_null_appearance(&field)? {
            return Ok(Some((start, field)));
        }
        let Some(kids) = self.object_array(field.get(b"Kids".as_slice()).cloned())? else {
            return Ok(None);
        };
        for kid in &kids {
            if let Object::Dictionary(widget) = kid {
                // A direct widget is an array item in its own right. Return
                // control to the direct-child updater here so it is selected
                // at this exact `/Kids` position rather than after later
                // indirect widgets.
                if self.has_non_null_appearance(widget)? {
                    return Ok(None);
                }
                continue;
            }
            let Object::Reference(kid_ref) = kid else {
                continue;
            };
            let Some(kid) = self.dictionary_handle_for(*kid_ref)? else {
                continue;
            };
            let annotation_ref = kid.object_ref().unwrap_or(*kid_ref);
            let kid = kid
                .materialize()
                .into_dict()
                .expect("dictionary handle must materialize as a dictionary");
            if self.has_non_null_appearance(&kid)? {
                return Ok(Some((annotation_ref, kid)));
            }
        }
        Ok(None)
    }

    /// Radio-button mutation follows qpdf's candidate selection: any
    /// non-null `/AP` identifies a widget, even when it is malformed and can
    /// therefore only receive an `/AS /Off` state.
    fn has_non_null_appearance(&mut self, dictionary: &Dictionary) -> Result<bool> {
        let Some(value) = dictionary.get(b"AP".as_slice()).cloned() else {
            return Ok(false);
        };
        let value = self.pdf.lift_object_to_handle(&value)?;
        let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
        Ok(!value.is_null())
    }

    fn normal_appearance_names(&mut self, dictionary: &Dictionary) -> Result<Vec<Vec<u8>>> {
        let Some(appearance) = dictionary.get(b"AP".as_slice()).cloned() else {
            return Ok(Vec::new()); // cov:ignore: every caller first requires a non-null /AP
        };
        let appearance = self.resolve_object(appearance)?;
        let Some(normal) = appearance
            .as_dict()
            .and_then(|dictionary| dictionary.get(b"N".as_slice()))
            .cloned()
        else {
            return Ok(Vec::new());
        };
        let normal = self.resolve_object(normal)?;
        Ok(normal
            .as_dict()
            .map(|dictionary| dictionary.iter().map(|(key, _)| key.to_vec()).collect())
            .unwrap_or_default())
    }

    fn set_direct_attribute(
        &mut self,
        reference: ObjectRef,
        key: &[u8],
        value: Object,
    ) -> Result<()> {
        let Some(dictionary) = self.dictionary_handle_for(reference)? else {
            return Ok(());
        };
        let value = self.pdf.lift_object_to_handle(&value)?;
        self.pdf.mark_object_handle_dirty(&dictionary)?;
        dictionary.replace_key(key, value);
        Ok(())
    }

    fn field_dict_for(&mut self, reference: ObjectRef, label: &str) -> Result<Dictionary> {
        match self.dictionary_handle_for(reference)? {
            Some(dictionary) => Ok(dictionary
                .materialize()
                .into_dict()
                .expect("dictionary handle must materialize as a dictionary")),
            // cov:ignore-start: all private callers establish a dictionary before mutation
            None => Err(Error::Unsupported(format!(
                "{label} object {reference} is not a dictionary"
            ))),
            // cov:ignore-end
        }
    }

    /// Return the canonical terminal dictionary handle for an indirect field
    /// holder. qpdf mutators operate on the dereferenced handle, so replacing
    /// a key must dirty the terminal object rather than an intermediate
    /// redirect recorded by flpdf's legacy object representation.
    fn dictionary_handle_for(&mut self, reference: ObjectRef) -> Result<Option<ObjectHandle>> {
        let start = self.pdf.get_object_handle(reference);
        let (resolved, terminal_ref) = self.pdf.resolve_object_handle_to_terminal_ref(&start)?;
        if resolved.as_dictionary().is_none() {
            return Ok(None);
        }
        let Some(terminal_ref) = terminal_ref else {
            return Ok(Some(resolved)); // cov:ignore: ObjectRef start always retains a terminal ref
        };
        let terminal = self.pdf.get_object_handle(terminal_ref);
        self.pdf.resolve_object_handle(&terminal)?;
        Ok(terminal.as_dictionary().map(|_| terminal))
    }

    fn resolve_object(&mut self, value: Object) -> Result<Object> {
        Ok(resolve_ref_chain(self.pdf, &value)?.0)
    }

    /// qpdf's `getKey` returns a null handle for an absent key, an explicit
    /// null, or a reference resolving to null. Keep that distinction at the
    /// helper boundary so button mutation uses the same top-level check.
    fn value_is_null(&mut self, value: Option<Object>) -> Result<bool> {
        match value {
            Some(value) => {
                let value = self.pdf.lift_object_to_handle(&value)?;
                let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
                Ok(value.is_null())
            }
            None => Ok(true),
        }
    }

    /// Resolve one `/Kids` holder object as qpdf's `getKey` does.
    fn object_array(&mut self, value: Option<Object>) -> Result<Option<Vec<Object>>> {
        let Some(value) = value else {
            return Ok(None);
        };
        Ok(self
            .resolve_array_target(value)?
            .map(|(items, _terminal_ref)| items))
    }

    /// Resolve an array value and preserve the terminal reference that owns
    /// it. A cyclic redirect degrades to null through the shared canonical
    /// handle resolver, matching qpdf's non-recursive `isArray()` access.
    fn resolve_array_target(
        &mut self,
        value: Object,
    ) -> Result<Option<(Vec<Object>, Option<ObjectRef>)>> {
        let value = self.pdf.lift_object_to_handle(&value)?;
        let (value, terminal_ref) = self.pdf.resolve_object_handle_to_terminal_ref(&value)?;
        let Object::Array(items) = value.materialize() else {
            return Ok(None);
        };
        Ok(Some((items, terminal_ref)))
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
        let acroform = resolve_ref_chain(self.pdf, &acroform)?.0;
        let Some(value) = acroform.as_dict().and_then(|dict| dict.get(key)).cloned() else {
            return Ok(None);
        };
        let value = resolve_ref_chain(self.pdf, &value)?.0;
        Ok((!matches!(value, Object::Null)).then_some(value))
    }

    fn utf8_string(value: &[u8]) -> String {
        let value = crate::pdf_string::utf8_value(value);
        String::from_utf8_lossy(&value).into_owned()
    }

    fn direct_parent(&mut self, field_ref: ObjectRef) -> Result<Option<ObjectRef>> {
        let node = self.pdf.get_object_handle(field_ref);
        let node = self.pdf.resolve_object_handle_to_terminal(&node)?;
        let Some(parent) = node
            .as_dictionary()
            .and_then(|dict| dict.get(b"Parent".as_slice()).cloned())
        else {
            return Ok(None);
        };
        let parent_ref = parent.object_ref().or_else(|| parent.as_reference());
        let parent = self.pdf.resolve_object_handle_to_terminal(&parent)?;
        Ok((!parent.is_null()).then_some(parent_ref).flatten())
    }

    fn resolve_inherited_name(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let Some(value) = self.resolve_inherited_handle(key)? else {
            return Ok(None);
        };
        let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
        Ok(value.as_name())
    }

    fn resolve_inherited_handle(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        let mut seen = BTreeSet::new();
        let mut current = self.field_ref;
        loop {
            if !seen.insert(current) {
                return Ok(None);
            }
            let node = self.pdf.get_object_handle(current);
            let node = self.pdf.resolve_object_handle_to_terminal(&node)?;
            let Some(dict) = node.as_dictionary() else {
                return Ok(None);
            };
            let found = dict.get(key).cloned();
            let parent_ref = dict
                .get(b"Parent".as_slice())
                .and_then(|parent| parent.object_ref().or_else(|| parent.as_reference()));
            if let Some(value) = found {
                let terminal = self.pdf.resolve_object_handle_to_terminal(&value)?;
                if !terminal.is_null() {
                    return Ok(Some(value));
                }
            }
            match parent_ref {
                Some(parent) => {
                    current = parent;
                }
                None => return Ok(None),
            }
        }
    }

    fn resolve_inherited_object(&mut self, key: &[u8]) -> Result<Option<Object>> {
        let Some(value) = self.resolve_inherited_handle(key)? else {
            return Ok(None);
        };
        let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
        Ok(Some(value.materialize()))
    }

    fn resolve_inherited_integer(&mut self, key: &[u8]) -> Result<Option<i64>> {
        let Some(value) = self.resolve_inherited_handle(key)? else {
            return Ok(None);
        };
        let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
        Ok(Some(value.as_integer().unwrap_or(0)))
    }

    fn string_key(&mut self, field_ref: ObjectRef, key: &[u8]) -> Result<Option<String>> {
        let node = self.pdf.get_object_handle(field_ref);
        let node = self.pdf.resolve_object_handle_to_terminal(&node)?;
        let Some(dict) = node.as_dictionary() else {
            return Ok(None);
        };
        let value = dict.get(key).cloned();
        self.resolve_string_handle(value)
    }

    fn resolve_string_handle(&mut self, value: Option<ObjectHandle>) -> Result<Option<String>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let value = self.pdf.resolve_object_handle_to_terminal(&value)?;
        Ok(value.as_string().map(|value| {
            String::from_utf8_lossy(&crate::pdf_string::utf8_value(&value)).into_owned()
        }))
    }
}
