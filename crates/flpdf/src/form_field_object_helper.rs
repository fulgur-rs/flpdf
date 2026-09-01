//! qpdf correspondence: `QPDFFormFieldObjectHelper.cc`.
//!
//! This module keeps form-field lookup and mutation on qpdf-shaped live
//! [`ObjectHandle`] values. In particular, a field helper never snapshots a
//! dictionary and writes a reconstructed value back through a legacy reader
//! cache: qpdf's helper owns the selected handle and mutates the
//! object graph that every other handle in the document observes.
//!
//! Parent-chain cycle detection follows qpdf's `QPDFObjGen::set`: indirect
//! object identities are keyed by `ObjectRef` in a `BTreeSet` (O(log n) per
//! check), while direct handles are not inserted
//! (`include/qpdf/QPDFObjGen.hh:105-124`). This mirrors qpdf exactly for
//! indirect cycles, but qpdf's own guard has the same gap flpdf would
//! otherwise inherit: a direct object's `QPDFObjGen` is always `(0, 0)`, so
//! `QPDFObjGen::set::add` unconditionally returns `true` for it and never
//! terminates a `/Parent` chain built entirely from direct dictionaries that
//! reciprocally reference each other (`include/qpdf/QPDFObjGen.hh:112-120`).
//! Such a chain cannot come from parsing real PDF bytes -- direct values are
//! inline text, so two of them cannot mutually contain each other in a
//! finite file -- but it is reachable in memory through the public
//! [`ObjectHandle::replace_key`] API, whose own doc already records that
//! gap. Unlike qpdf's C++ walk, this crate's walk must not hang the hosting
//! process for that shape.
//!
//! The guard for that gap tracks direct handles by live identity
//! (`ObjectHandle::is_same_object_as`) in a side list, checked only when the
//! current node is direct. This is a bound on an actual repeat, not on
//! depth: a direct `/Parent` chain has no upper bound here other than a
//! genuine cycle, matching qpdf's own unbounded-but-for-cycles walk for
//! indirect handles. Direct
//! `/Parent` chains are rare in practice, so the linear scan this implies
//! (O(n) per direct node, O(n^2) for a long acyclic direct-only chain) is
//! accepted rather than adding new machinery for a shape indirect objects
//! already avoid via the `BTreeSet`.

use crate::object_handle::ObjectHandle;
use crate::{Error, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

#[path = "form_field_object_helper/rendering.rs"]
mod rendering;

fn mark_field_node_seen(seen: &mut BTreeSet<ObjectRef>, current: &ObjectHandle) -> bool {
    current
        .object_ref()
        .map(|object_ref| seen.insert(object_ref))
        .unwrap_or(true)
}

/// Detect an actual repeat among **direct** `/Parent` nodes by live handle
/// identity. Indirect nodes are ignored here (they are already turned into
/// termination by the `ObjectRef` `seen` set); a direct node not yet seen is
/// recorded and the walk continues. See the module doc for why direct nodes
/// need this separate, identity-based guard.
fn mark_direct_node_seen(direct_seen: &mut Vec<ObjectHandle>, current: &ObjectHandle) -> bool {
    if !current.is_direct() {
        return true;
    }
    if direct_seen
        .iter()
        .any(|handle: &ObjectHandle| handle.is_same_object_as(current))
    {
        return false;
    }
    direct_seen.push(current.clone());
    true
}

/// The bounded error raised when [`mark_direct_node_seen`] detects an actual
/// repeat. Real PDF bytes cannot produce this shape (see the module doc), so
/// this is unreachable from a live qpdf parse and exists only to bound the
/// in-memory `ObjectHandle::replace_key` gap.
fn direct_parent_cycle_error(field_ref: ObjectRef) -> Error {
    Error::Unsupported(format!(
        "field tree contains a /Parent cycle of direct dictionaries at {field_ref}"
    ))
}

/// Typed access helper for a PDF AcroForm field or widget annotation
/// dictionary.
pub struct FormFieldObjectHelper<'a, R: Read + Seek + 'static> {
    field_ref: Option<ObjectRef>,
    field: ObjectHandle,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FormFieldObjectHelper<'a, R> {
    /// Construct a helper for the field at `field_ref`.
    ///
    /// The object is looked up through the document's canonical registry once;
    /// all later access is through this same live handle, matching qpdf's
    /// `QPDFFormFieldObjectHelper(QPDFObjectHandle)` constructor.
    pub fn new(field_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        let field = pdf.get_object_handle(field_ref);
        Self {
            field_ref: Some(field_ref),
            field,
            pdf,
        }
    }

    /// Construct a helper from a live field handle, preserving direct-object
    /// identity for qpdf's orphan-Widget fallback.
    pub(crate) fn from_object_handle(field: ObjectHandle, pdf: &'a mut Pdf<R>) -> Self {
        Self {
            field_ref: field.object_ref(),
            field,
            pdf,
        }
    }

    /// Return whether the referenced field object is PDF null.
    pub fn is_null(&mut self) -> Result<bool> {
        let field = self.resolved(self.field.clone())?;
        Ok(field.is_null())
    }

    /// Return this field's direct `/Parent` reference, if present.
    pub fn parent(&mut self) -> Result<Option<ObjectRef>> {
        self.direct_parent(self.field.clone())
    }

    /// Return the top-level field and whether it differs from this field.
    ///
    /// Mirrors qpdf's `QPDFFormFieldObjectHelper::getTopLevelField`
    /// (`libqpdf/QPDFFormFieldObjectHelper.cc:35-46`), which climbs `/Parent`
    /// until a cycle or a terminal node with no upper bound on depth (its
    /// guard is `QPDFObjGen::set`, a pure cycle detector). This walk shares
    /// that same cycle-only termination via `mark_field_node_seen` --
    /// unlike `resolve_inherited_handle_from`, `current` here is always
    /// indirect (this function stops rather than climbs into a direct
    /// `/Parent`, since its `ObjectRef`-typed return cannot represent a
    /// direct top field), so the module's separate direct-handle cycle guard
    /// does not apply.
    pub fn get_top_level_field(&mut self) -> Result<(ObjectRef, bool)> {
        let Some(field_ref) = self.field_ref else {
            return Err(Error::Unsupported(
                "direct field has no ObjectRef top-level identity".to_string(),
            ));
        };
        let mut current = self.field.clone();
        let mut seen = BTreeSet::new();
        let mut top = field_ref;
        let mut is_different = false;

        loop {
            if !mark_field_node_seen(&mut seen, &current) {
                break;
            }

            let node = self.resolved(current.clone())?;
            let parent = node.get_key(b"/Parent");
            let parent = self.resolved(parent)?;
            if parent.is_null() {
                break;
            }
            let Some(parent_ref) = parent.object_ref() else {
                break;
            };
            top = parent_ref;
            current = parent;
            is_different = true;
        }

        Ok((top, is_different))
    }

    /// Return an inheritable field value while preserving the selected
    /// qpdf-style handle identity.
    pub fn inheritable_value(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        self.resolve_inherited_handle(key)
    }

    /// Return an inheritable PDF string as qpdf-style UTF-8 text.
    pub fn inheritable_string(&mut self, key: &[u8]) -> Result<String> {
        let value = self.resolve_inherited_handle(key)?;
        Ok(self.resolve_string_handle(value)?.unwrap_or_default())
    }

    /// Return an inheritable PDF name with its leading slash.
    pub fn inheritable_name(&mut self, key: &[u8]) -> Result<Vec<u8>> {
        Ok(self
            .resolve_inherited_name(self.field.clone(), key)?
            .map(|name| {
                let mut result = Vec::with_capacity(name.len() + 1);
                result.push(b'/');
                result.extend(name);
                result
            })
            .unwrap_or_default())
    }

    /// Return the inheritable `/FT` field type as qpdf-style name bytes,
    /// including the leading slash.
    pub fn field_type(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self
            .resolve_inherited_name(self.field.clone(), b"FT")?
            .map(|name| {
                let mut result = Vec::with_capacity(name.len() + 1);
                result.push(b'/');
                result.extend(name);
                result
            }))
    }

    /// Return the inheritable `/V` field value without discarding indirect
    /// identity.
    pub fn field_value(&mut self) -> Result<Option<ObjectHandle>> {
        self.resolve_inherited_handle(b"V")
    }

    /// Return the inheritable `/V` handle.
    pub fn field_value_handle(&mut self) -> Result<Option<ObjectHandle>> {
        self.field_value()
    }

    /// Return the indirect object reference selected by inherited `/V`.
    pub fn field_value_reference(&mut self) -> Result<Option<ObjectRef>> {
        Ok(self.field_value()?.and_then(|value| value.object_ref()))
    }

    /// Return the inheritable `/DV` field default value without discarding
    /// indirect identity.
    pub fn field_default_value(&mut self) -> Result<Option<ObjectHandle>> {
        self.resolve_inherited_handle(b"DV")
    }

    /// Return the inheritable `/DV` handle.
    pub fn field_default_value_handle(&mut self) -> Result<Option<ObjectHandle>> {
        self.field_default_value()
    }

    /// Return the inheritable `/V` field value.
    pub fn value(&mut self) -> Result<Option<ObjectHandle>> {
        self.field_value()
    }

    /// Return the inheritable `/V` as qpdf-style UTF-8 text.
    pub fn value_as_string(&mut self) -> Result<String> {
        self.inheritable_string(b"V")
    }

    /// Return the inheritable `/DV` field default value.
    pub fn default_value(&mut self) -> Result<Option<ObjectHandle>> {
        self.field_default_value()
    }

    /// Return the inheritable `/DV` as qpdf-style UTF-8 text.
    pub fn default_value_as_string(&mut self) -> Result<String> {
        self.inheritable_string(b"DV")
    }

    /// Return the inheritable `/Ff` field flags.
    pub fn field_flags(&mut self) -> Result<Option<i64>> {
        self.resolve_inherited_integer(self.field.clone(), b"Ff")
    }

    /// Return the inheritable `/Ff` flags, defaulting to zero.
    pub fn flags(&mut self) -> Result<i64> {
        Ok(self.field_flags()?.unwrap_or(0))
    }

    /// Return this field's `/T` partial name as qpdf-style UTF-8 text.
    pub fn partial_name(&mut self) -> Result<String> {
        self.string_key(self.field.clone(), b"T")
            .map(|value| value.unwrap_or_default())
    }

    /// Return the dotted `/T` name formed by this field and its parents.
    pub fn fully_qualified_name(&mut self) -> Result<String> {
        let mut current = self.field.clone();
        let mut seen = BTreeSet::new();
        let mut direct_seen = Vec::new();
        let mut parts = Vec::new();

        while mark_field_node_seen(&mut seen, &current) {
            if !mark_direct_node_seen(&mut direct_seen, &current) {
                return Err(direct_parent_cycle_error(
                    self.field_ref.unwrap_or(ObjectRef::new(0, 0)),
                ));
            }
            let node = self.resolved(current.clone())?;
            if node.as_dictionary().is_none() {
                break;
            }
            if let Some(name) = self.resolve_string_handle(Some(node.get_key(b"/T")))? {
                parts.push(name);
            }
            let parent = self.resolved(node.get_key(b"/Parent"))?;
            if parent.is_null() {
                break;
            }
            current = parent;
        }

        parts.reverse();
        Ok(parts.join("."))
    }

    /// Return `/TU`, or the fully qualified name when `/TU` is absent.
    pub fn alternative_name(&mut self) -> Result<String> {
        match self.string_key(self.field.clone(), b"TU")? {
            Some(name) => Ok(name),
            None => self.fully_qualified_name(),
        }
    }

    /// Return `/TM`, then `/TU`, then the fully qualified name.
    pub fn mapping_name(&mut self) -> Result<String> {
        match self.string_key(self.field.clone(), b"TM")? {
            Some(name) => Ok(name),
            None => self.alternative_name(),
        }
    }

    /// Return the default appearance string, inheriting `/DA` from the field
    /// tree and then falling back to `/AcroForm`.
    pub fn default_appearance(&mut self) -> Result<String> {
        if let Some(value) = self.resolve_inherited_handle(b"DA")? {
            if let Some(value) = self.resolve_string_handle(Some(value))? {
                return Ok(value);
            }
        }
        let value = self.acroform_value(b"DA")?;
        Ok(self.resolve_string_handle(value)?.unwrap_or_default())
    }

    /// Return the document-level `/AcroForm/DR` handle.
    ///
    /// qpdf deliberately does not inherit `/DR` through the field tree.
    pub fn default_resources(&mut self) -> Result<Option<ObjectHandle>> {
        self.acroform_value(b"DR")
    }

    /// Return the quadding value, inheriting `/Q` and then falling back to
    /// `/AcroForm/Q`. Missing or non-integer values are zero as in qpdf.
    pub fn quadding(&mut self) -> Result<i64> {
        if let Some(value) = self.resolve_inherited_handle(b"Q")? {
            if let Some(value) = self.resolved(value)?.as_integer() {
                return Ok(value);
            }
        }
        Ok(self
            .acroform_value(b"Q")?
            .map(|value| self.resolved(value))
            .transpose()?
            .and_then(|value| value.as_integer())
            .unwrap_or(0))
    }

    /// Return whether this field is a text field (`/FT /Tx`).
    pub fn is_text(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Tx"))
    }

    /// Return whether this field is a checkbox button.
    pub fn is_checkbox(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Btn")
            && self.field_flags()?.unwrap_or(0) & ((1 << 15) | (1 << 16)) == 0)
    }

    /// Return whether this checkbox has an on-state value.
    pub fn is_checked(&mut self) -> Result<bool> {
        if !self.is_checkbox()? {
            return Ok(false);
        }
        let Some(value) = self.field_value()? else {
            return Ok(false);
        };
        let value = self.resolved(value)?;
        Ok(value.as_name().is_some_and(|value| value != b"Off"))
    }

    /// Return whether this field is a radio button (`/Btn`, flag bit 16).
    pub fn is_radio_button(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Btn")
            && self.field_flags()?.unwrap_or(0) & (1 << 15) == 1 << 15)
    }

    /// Return whether this field is a pushbutton (`/Btn`, flag bit 17).
    pub fn is_pushbutton(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Btn")
            && self.field_flags()?.unwrap_or(0) & (1 << 16) == 1 << 16)
    }

    /// Return whether this field is a choice field (`/FT /Ch`).
    pub fn is_choice(&mut self) -> Result<bool> {
        Ok(self.field_type()?.as_deref() == Some(b"/Ch"))
    }

    /// Return qpdf's choice labels for a choice field.
    pub fn choices(&mut self) -> Result<Vec<String>> {
        if !self.is_choice()? {
            return Ok(Vec::new());
        }
        let Some(options) = self.resolve_inherited_handle(b"Opt")? else {
            return Ok(Vec::new());
        };
        let options = self.resolved(options)?;
        let Some(items) = options.as_array() else {
            return Ok(Vec::new());
        };

        let mut choices = Vec::new();
        for item in items {
            if let Some(value) = self.resolve_string_handle(Some(item))? {
                choices.push(value);
            }
        }
        Ok(choices)
    }

    /// Replace a direct attribute on this field dictionary.
    pub fn set_field_attribute(&mut self, key: &[u8], value: ObjectHandle) -> Result<()> {
        self.set_direct_attribute(&self.field.clone(), key, value)
    }

    /// Replace a direct string attribute using qpdf's Unicode-string encoding.
    pub fn set_field_attribute_string(&mut self, key: &[u8], value: &str) -> Result<()> {
        self.set_field_attribute(
            key,
            ObjectHandle::string(crate::pdf_string::new_unicode_string(value.as_bytes())),
        )
    }

    /// Set the field's `/V` value using qpdf's form-field dispatch.
    pub fn set_value(&mut self, value: ObjectHandle, need_appearances: bool) -> Result<()> {
        let value = self.resolved(value)?;
        if self.field_type()?.as_deref() == Some(b"/Btn") {
            if let Some(name) = value.as_name() {
                if self.is_checkbox()? {
                    self.set_checkbox_value(name != b"Off")?;
                } else if self.is_radio_button()? {
                    self.set_radio_button_value(self.field.clone(), &name)?;
                }
            }
            // qpdf ignores invalid button input and pushbutton values.
            return Ok(());
        }

        let value = if let Some(string) = value.as_string() {
            ObjectHandle::string(crate::pdf_string::new_unicode_string(
                &crate::pdf_string::utf8_value(&string),
            ))
        } else {
            value
        };
        self.set_field_attribute(b"V", value)?;
        if need_appearances {
            self.set_need_appearances()?;
        }
        Ok(())
    }

    /// Set `/V` from UTF-8 text using qpdf's Unicode-string encoding.
    pub fn set_value_string(&mut self, value: &str, need_appearances: bool) -> Result<()> {
        self.set_value(
            ObjectHandle::string(crate::pdf_string::new_unicode_string(value.as_bytes())),
            need_appearances,
        )
    }

    /// Generate a normal appearance for `widget_ref` from this field's value.
    pub fn generate_appearance_for(&mut self, widget_ref: ObjectRef) -> Result<Option<ObjectRef>> {
        let widget = self.pdf.get_object_handle(widget_ref);
        self.generate_appearance_for_handle(widget)
    }

    /// Generate a normal appearance for a live Widget handle from this
    /// field's value. This is the handle-native counterpart of
    /// [`Self::generate_appearance_for`] and also accepts direct orphan
    /// widgets.
    pub(crate) fn generate_appearance_for_handle(
        &mut self,
        widget: ObjectHandle,
    ) -> Result<Option<ObjectRef>> {
        match self.field_type()?.as_deref() {
            Some(b"/Tx") => {
                rendering::render_text_field_canonical_handles(self.pdf, self.field.clone(), widget)
            }
            Some(b"/Ch") => rendering::render_choice_field_canonical_handles(
                self.pdf,
                self.field.clone(),
                widget,
            ),
            _ => Ok(None),
        }
    }

    /// Clear `/AcroForm/NeedAppearances` after a document appearance pass.
    pub fn clear_need_appearances_after_generation(pdf: &mut Pdf<R>) -> Result<()> {
        let root = pdf.trailer_key_handle(b"Root");
        pdf.resolve(&root)?;
        if root.is_null() {
            return Ok(());
        }
        let acroform = root.get_key(b"/AcroForm");
        pdf.resolve(&acroform)?;
        if acroform.as_dictionary().is_none() {
            return Ok(());
        }
        let need_appearances = acroform.get_key(b"/NeedAppearances");
        pdf.resolve(&need_appearances)?;
        if need_appearances.as_boolean() != Some(true) {
            return Ok(());
        }
        acroform.remove_key(b"/NeedAppearances");
        pdf.mark_object_handle_dirty(&acroform)
    }

    fn set_need_appearances(&mut self) -> Result<()> {
        crate::AcroFormDocumentHelper::new(self.pdf)?.set_need_appearances(true)
    }

    fn set_checkbox_value(&mut self, checked: bool) -> Result<()> {
        let annotation = self.appearance_annotation(self.field.clone())?;
        let on_value = self.checkbox_state(annotation.as_ref(), checked)?;
        let value = ObjectHandle::name(on_value);
        self.set_field_attribute(b"/V", value.clone())?;
        if let Some(annotation) = annotation {
            self.set_direct_attribute(&annotation, b"/AS", value)?;
        }
        Ok(())
    }

    fn checkbox_state(
        &mut self,
        annotation: Option<&ObjectHandle>,
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

    fn set_radio_button_value(&mut self, field: ObjectHandle, value: &[u8]) -> Result<()> {
        let field = self.resolved(field)?;

        let parent = self.resolved(field.get_key(b"/Parent"))?;
        if !parent.is_null() {
            if let Some(parent_dict) = self.dictionary_handle_for(parent.clone())? {
                let parent_parent = self.resolved(parent_dict.get_key(b"/Parent"))?;
                if parent_parent.is_null() && self.is_radio_for(parent_dict.clone())? {
                    return self.set_radio_button_value(parent_dict, value);
                }
            }
        }

        let parent_is_null = parent.is_null();
        let kids = self.resolved(field.get_key(b"/Kids"))?;
        if kids.as_array().is_none() || !parent_is_null {
            return Ok(());
        }

        self.set_direct_attribute(&field, b"/V", ObjectHandle::name(value.to_vec()))?;
        let Some(items) = kids.as_array() else {
            return Ok(());
        };
        self.update_radio_kids(items, value)
    }

    fn update_radio_kids(&mut self, kids: Vec<ObjectHandle>, value: &[u8]) -> Result<()> {
        for kid in kids {
            self.update_radio_kid(kid, value)?;
        }
        Ok(())
    }

    /// qpdf looks one level below a radio child that has no `/AP`; it does
    /// not recurse beyond that child field.
    fn update_radio_kid(&mut self, kid: ObjectHandle, value: &[u8]) -> Result<()> {
        let kid = self.resolved(kid)?;
        if kid.as_dictionary().is_none() {
            return Ok(());
        }
        if self.has_non_null_appearance(&kid)? {
            let state = self.radio_state(&kid, value)?;
            self.set_direct_attribute(&kid, b"/AS", ObjectHandle::name(state))?;
            return Ok(());
        }

        let grandkids = self.resolved(kid.get_key(b"/Kids"))?;
        if let Some(items) = grandkids.as_array() {
            self.update_first_radio_widget(items, value)?;
        }
        Ok(())
    }

    fn update_first_radio_widget(&mut self, kids: Vec<ObjectHandle>, value: &[u8]) -> Result<bool> {
        for widget in kids {
            if self.update_radio_widget(widget, value)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn update_radio_widget(&mut self, widget: ObjectHandle, value: &[u8]) -> Result<bool> {
        let widget = self.resolved(widget)?;
        if widget.as_dictionary().is_none() || !self.has_non_null_appearance(&widget)? {
            return Ok(false);
        }
        let state = self.radio_state(&widget, value)?;
        self.set_direct_attribute(&widget, b"/AS", ObjectHandle::name(state))?;
        Ok(true)
    }

    fn radio_state(&mut self, widget: &ObjectHandle, value: &[u8]) -> Result<Vec<u8>> {
        let names = self.normal_appearance_names(widget)?;
        Ok(if names.iter().any(|name| name == value) {
            value.to_vec()
        } else {
            b"Off".to_vec()
        })
    }

    /// Locate the widget annotation to mutate `/AS` on: the field itself if
    /// it carries a non-null `/AP`, else the first `/Kids` child that does.
    /// Mirrors qpdf's `setCheckBoxValue` (`QPDFFormFieldObjectHelper.cc:416-462`),
    /// which operates on the resulting `QPDFObjectHandle` (`annot`) directly
    /// regardless of whether it is a direct or indirect object -- unlike an
    /// earlier version of this helper, this must not require an object
    /// number, or a direct field/widget's `/AS` silently never gets synced.
    fn appearance_annotation(&mut self, start: ObjectHandle) -> Result<Option<ObjectHandle>> {
        let field = self.resolved(start)?;
        if self.has_non_null_appearance(&field)? {
            return Ok(Some(field));
        }

        let kids = self.resolved(field.get_key(b"/Kids"))?;
        let Some(items) = kids.as_array() else {
            return Ok(None);
        };
        for kid in items {
            let kid = self.resolved(kid)?;
            if kid.as_dictionary().is_none() {
                continue;
            }
            if self.has_non_null_appearance(&kid)? {
                return Ok(Some(kid));
            }
        }
        Ok(None)
    }

    /// Any non-null `/AP` makes a radio candidate a widget, even when its
    /// appearance dictionary is malformed.
    fn has_non_null_appearance(&mut self, dictionary: &ObjectHandle) -> Result<bool> {
        let appearance = self.resolved(dictionary.get_key(b"/AP"))?;
        Ok(!appearance.is_null())
    }

    fn normal_appearance_names(&mut self, dictionary: &ObjectHandle) -> Result<Vec<Vec<u8>>> {
        let appearance = self.resolved(dictionary.get_key(b"/AP"))?;
        let normal = self.resolved(appearance.get_key(b"/N"))?;
        let Some(entries) = normal.as_dictionary() else {
            return Ok(Vec::new());
        };
        Ok(entries
            .keys()
            .filter_map(|key| key.strip_prefix(b"/").map(ToOwned::to_owned))
            .collect())
    }

    fn set_direct_attribute(
        &mut self,
        target: &ObjectHandle,
        key: &[u8],
        value: ObjectHandle,
    ) -> Result<()> {
        let Some(dictionary) = self.dictionary_handle_for(target.clone())? else {
            return Ok(());
        };
        let key = crate::object_handle::canonical_dictionary_key(key);
        dictionary.replace_key(&key, value)?;
        self.pdf.mark_object_handle_dirty(&dictionary)
    }

    fn dictionary_handle_for(&mut self, handle: ObjectHandle) -> Result<Option<ObjectHandle>> {
        let handle = self.resolved(handle)?;
        Ok(handle.as_dictionary().map(|_| handle))
    }

    fn is_radio_for(&mut self, field: ObjectHandle) -> Result<bool> {
        Ok(
            self.field_type_for(field.clone())?.as_deref() == Some(b"/Btn")
                && self.resolve_inherited_integer(field, b"Ff")?.unwrap_or(0) & (1 << 15)
                    == 1 << 15,
        )
    }

    fn field_type_for(&mut self, field: ObjectHandle) -> Result<Option<Vec<u8>>> {
        Ok(self.resolve_inherited_name(field, b"FT")?.map(|name| {
            let mut result = Vec::with_capacity(name.len() + 1);
            result.push(b'/');
            result.extend(name);
            result
        }))
    }

    fn acroform_value(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        let root = self.pdf.trailer_key_handle(b"Root");
        let root = self.resolved(root)?;
        if root.is_null() || root.as_dictionary().is_none() {
            return Ok(None);
        }
        let acroform = self.resolved(root.get_key(b"/AcroForm"))?;
        if acroform.as_dictionary().is_none() {
            return Ok(None);
        }
        let value =
            self.resolved(acroform.get_key(&crate::object_handle::canonical_dictionary_key(key)))?;
        // A live qpdf parse never stores a bare reference as an object's
        // value. Keep the document-level accessor on the same null/dictionary
        // boundary as qpdf without exposing any separate reference value.
        Ok((!value.is_null()).then_some(value))
    }

    fn utf8_string(value: &[u8]) -> String {
        String::from_utf8_lossy(&crate::pdf_string::utf8_value(value)).into_owned()
    }

    fn direct_parent(&mut self, field: ObjectHandle) -> Result<Option<ObjectRef>> {
        let field = self.resolved(field)?;
        if field.as_dictionary().is_none() {
            return Ok(None);
        }
        let parent = self.resolved(field.get_key(b"/Parent"))?;
        if parent.is_null() {
            return Ok(None);
        }
        Ok(parent.object_ref())
    }

    fn resolve_inherited_name(
        &mut self,
        field: ObjectHandle,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let Some(value) = self.resolve_inherited_handle_from(field, key)? else {
            return Ok(None);
        };
        Ok(self.resolved(value)?.as_name())
    }

    fn resolve_inherited_handle(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        self.resolve_inherited_handle_from(self.field.clone(), key)
    }

    /// Walk field nodes by live handle identity.  qpdf's seen set is over
    /// object identity, not over a materialized reference spelling, so direct
    /// children and indirect children both continue the walk the same way;
    /// only their termination guard differs -- see the module doc for why.
    fn resolve_inherited_handle_from(
        &mut self,
        field: ObjectHandle,
        key: &[u8],
    ) -> Result<Option<ObjectHandle>> {
        let key = crate::object_handle::canonical_dictionary_key(key);
        let mut current = field;
        let mut seen = BTreeSet::new();
        let mut direct_seen = Vec::new();
        loop {
            if !mark_field_node_seen(&mut seen, &current) {
                return Ok(None);
            }
            if !mark_direct_node_seen(&mut direct_seen, &current) {
                return Err(direct_parent_cycle_error(
                    self.field_ref.unwrap_or(ObjectRef::new(0, 0)),
                ));
            }

            let node = self.resolved(current)?;
            if node.as_dictionary().is_none() {
                return Ok(None);
            }
            let value = self.resolved(node.get_key(&key))?;
            if !value.is_null() {
                return Ok(Some(value));
            }

            let parent = self.resolved(node.get_key(b"/Parent"))?;
            if parent.is_null() {
                return Ok(None);
            }
            current = parent;
        }
    }

    fn resolve_inherited_integer(
        &mut self,
        field: ObjectHandle,
        key: &[u8],
    ) -> Result<Option<i64>> {
        let Some(value) = self.resolve_inherited_handle_from(field, key)? else {
            return Ok(None);
        };
        Ok(Some(self.resolved(value)?.as_integer().unwrap_or(0)))
    }

    fn string_key(&mut self, field: ObjectHandle, key: &[u8]) -> Result<Option<String>> {
        let field = self.resolved(field)?;
        if field.as_dictionary().is_none() {
            return Ok(None);
        }
        self.resolve_string_handle(Some(
            field.get_key(&crate::object_handle::canonical_dictionary_key(key)),
        ))
    }

    fn resolve_string_handle(&mut self, value: Option<ObjectHandle>) -> Result<Option<String>> {
        let Some(value) = value else {
            return Ok(None);
        };
        let value = self.resolved(value)?;
        Ok(value.as_string().map(|value| Self::utf8_string(&value)))
    }

    fn resolved(&mut self, handle: ObjectHandle) -> Result<ObjectHandle> {
        self.pdf.resolve(&handle)?;
        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::mark_field_node_seen;
    use crate::object_handle::ObjectHandle;
    use crate::{ObjectRef, Pdf};
    use std::collections::BTreeSet;

    #[test]
    fn qpdf_seen_set_ignores_direct_handles_and_tracks_indirect_identity() {
        let mut seen = BTreeSet::new();

        assert!(mark_field_node_seen(&mut seen, &ObjectHandle::integer(1)));
        assert!(mark_field_node_seen(&mut seen, &ObjectHandle::integer(1)));

        let first = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        let same_object = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        assert!(mark_field_node_seen(&mut seen, &first));
        assert!(!mark_field_node_seen(&mut seen, &same_object));
    }

    #[test]
    fn direct_seen_set_ignores_indirect_handles_and_tracks_direct_identity() {
        let mut direct_seen = Vec::new();

        // Indirect handles are ignored -- the `BTreeSet` in
        // `mark_field_node_seen` already owns their identity.
        let indirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(10, 0), -1);
        assert!(super::mark_direct_node_seen(&mut direct_seen, &indirect));
        assert!(super::mark_direct_node_seen(&mut direct_seen, &indirect));
        assert!(direct_seen.is_empty());

        // A direct handle is recorded the first time and rejected on an
        // actual repeat of the same underlying allocation.
        let direct = ObjectHandle::dictionary(Vec::new());
        assert!(super::mark_direct_node_seen(&mut direct_seen, &direct));
        assert!(!super::mark_direct_node_seen(&mut direct_seen, &direct));

        // A different direct handle with equal contents but distinct
        // identity is not a repeat.
        let other_direct = ObjectHandle::dictionary(Vec::new());
        assert!(super::mark_direct_node_seen(
            &mut direct_seen,
            &other_direct
        ));
    }

    #[test]
    fn direct_field_has_no_top_level_object_reference() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let mut helper = super::FormFieldObjectHelper::from_object_handle(
            ObjectHandle::dictionary(Vec::new()),
            &mut pdf,
        );

        let error = helper
            .get_top_level_field()
            .expect_err("direct field cannot report an indirect top-level reference");
        assert!(
            matches!(error, crate::Error::Unsupported(message) if message.contains("no ObjectRef"))
        );
    }

    #[test]
    fn set_checkbox_value_syncs_as_for_a_direct_merged_field_widget() {
        // qpdf's `setCheckBoxValue` (`QPDFFormFieldObjectHelper.cc:416-462`)
        // operates on `this->oh` directly with no dependency on whether it
        // has an object number -- a checkbox where the field and widget are
        // merged into one direct dictionary (no /Kids, /AP on the field
        // itself) must still get /AS synced, not just /V.
        let mut pdf = Pdf::empty().expect("empty PDF");
        let field = ObjectHandle::dictionary(vec![
            (b"/FT".to_vec(), ObjectHandle::name(b"Btn".to_vec())),
            (
                b"/AP".to_vec(),
                ObjectHandle::dictionary(vec![(
                    b"/N".to_vec(),
                    ObjectHandle::dictionary(vec![
                        (b"/Off".to_vec(), ObjectHandle::null()),
                        (b"/Chosen".to_vec(), ObjectHandle::null()),
                    ]),
                )]),
            ),
        ]);
        let mut helper = super::FormFieldObjectHelper::from_object_handle(field.clone(), &mut pdf);
        helper
            .set_value(ObjectHandle::name(b"On".to_vec()), true)
            .expect("set direct merged checkbox value");

        assert_eq!(
            field.try_get_key(b"/V").unwrap().as_name(),
            Some(b"Chosen".to_vec())
        );
        assert_eq!(
            field.try_get_key(b"/AS").unwrap().as_name(),
            Some(b"Chosen".to_vec()),
            "a direct merged field/widget checkbox must have /AS synced, not just /V"
        );
    }
}
