# QPDFFormFieldObjectHelper oracle matrix

**Oracle:** the pinned qpdf 11.9.0 worktree resolved by
`scripts/fetch-qpdf-source.sh --print-path`. The Rust helper is the sole public
form-field boundary: `flpdf::form_field_object_helper::FormFieldObjectHelper`.
It owns a mutable `Pdf` borrow and an `ObjectRef`; every fallible operation
therefore returns `flpdf::Result<_>`.

| qpdf public API group | qpdf declaration and definition | Intended Rust method or group |
| --- | --- | --- |
| Construction and null identity: constructors, `isNull()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:35-43`; constructor definitions `libqpdf/QPDFFormFieldObjectHelper.cc:11-26` | `FormFieldObjectHelper::new(ObjectRef, &mut Pdf)` and `is_null() -> Result<bool>`. Rust has no default/null helper constructor; `is_null` observes whether the referenced field resolves to PDF null. |
| Parent and top-level traversal: `getParent()`, `getTopLevelField(bool*)` | `include/qpdf/QPDFFormFieldObjectHelper.hh:45-54`; `libqpdf/QPDFFormFieldObjectHelper.cc:30-46` | `parent() -> Result<Option<ObjectRef>>` and `top_level_field() -> Result<(ObjectRef, bool)>`; the boolean is qpdf's `is_different` out-parameter. |
| Raw inheritable lookup: `getInheritableFieldValue(name)` | `include/qpdf/QPDFFormFieldObjectHelper.hh:56-58`; `libqpdf/QPDFFormFieldObjectHelper.cc:66-85` | `inheritable_value(key: &[u8]) -> Result<Option<Object>>`, which is the shared `/Parent` resolver. |
| Typed inheritable lookup: `getInheritableFieldValueAsString(name)`, `getInheritableFieldValueAsName(name)` | `include/qpdf/QPDFFormFieldObjectHelper.hh:60-68`; `libqpdf/QPDFFormFieldObjectHelper.cc:88-107` | `inheritable_string(key: &[u8]) -> Result<String>` and `inheritable_name(key: &[u8]) -> Result<Vec<u8>>`; nonmatching types use qpdf's empty result convention. |
| Field type: `getFieldType()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:70-72`; `libqpdf/QPDFFormFieldObjectHelper.cc:110-113` | `field_type() -> Result<Option<Vec<u8>>>`; raw PDF name bytes are retained at the Rust boundary. |
| Field names: `getFullyQualifiedName()`, `getPartialName()`, `getAlternativeName()`, `getMappingName()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:74-88`; `libqpdf/QPDFFormFieldObjectHelper.cc:116-164` | `fully_qualified_name()`, `partial_name()`, `alternative_name()`, and `mapping_name()`, all returning `Result<String>`. `mapping_name()` returns `/TM` when present; otherwise it follows `getAlternativeName()` (`cc:156-164`): `/TU`, then the fully-qualified `/T` parent-chain name — never merely the partial name. |
| Value and default value, raw and string forms: `getValue()`, `getValueAsString()`, `getDefaultValue()`, `getDefaultValueAsString()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:90-104`; `libqpdf/QPDFFormFieldObjectHelper.cc:167-188` | `value()` / `default_value()` return `Result<Option<Object>>`; `value_as_string()` / `default_value_as_string()` return `Result<String>`. |
| AcroForm/field metadata: `getDefaultAppearance()`, `getDefaultResources()`, `getQuadding()`, `getFlags()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:106-128`; `libqpdf/QPDFFormFieldObjectHelper.cc:191-235` (the shared AcroForm lookup is `:50-63`) | `default_appearance() -> Result<String>`, `default_resources() -> Result<Option<Object>>`, `quadding() -> Result<i64>`, and `flags() -> Result<i64>`. `/DA` and `/Q` first inherit then fall back to `/AcroForm`; `/DR` comes only from `/AcroForm`. |
| Type predicates: `isText()`, `isCheckbox()`, `isChecked()`, `isRadioButton()`, `isPushbutton()`, `isChoice()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:130-149`; definitions for all except `isChecked` are `libqpdf/QPDFFormFieldObjectHelper.cc:238-265`. qpdf 11.9.0 declares `isChecked` but has no matching definition in that pinned `.cc` file. | `is_text()`, `is_checkbox()`, `is_checked()`, `is_radio_button()`, `is_pushbutton()`, and `is_choice()`, each returning `Result<bool>`. `is_checked` remains part of Rust's public parity surface and will be specified by checkbox state tests; its declaration-only status in qpdf 11.9.0 must not be hidden. |
| Choice values: `getChoices()` | `include/qpdf/QPDFFormFieldObjectHelper.hh:150-152`; `libqpdf/QPDFFormFieldObjectHelper.cc:268-285` | `choices() -> Result<Vec<String>>`, using inherited `/Opt` only for `/Ch`. |
| Attribute setters: `setFieldAttribute(key, QPDFObjectHandle)` and `setFieldAttribute(key, utf8)` | `include/qpdf/QPDFFormFieldObjectHelper.hh:154-163`; `libqpdf/QPDFFormFieldObjectHelper.cc:288-297` | `set_field_attribute(key: &[u8], value: Object)` and `set_field_attribute_string(key: &[u8], utf8_value: &str)`, both returning `Result<()>`; the string form encodes a PDF Unicode string. |
| Value setters: `setV(QPDFObjectHandle, bool)`, `setV(utf8, bool)` | `include/qpdf/QPDFFormFieldObjectHelper.hh:165-178`; `libqpdf/QPDFFormFieldObjectHelper.cc:300-345`, with button-specific helpers at `:348-469` | `set_value(value: Object, need_appearances: bool)` and `set_value_string(utf8_value: &str, need_appearances: bool)`, returning `Result<()>`. The helper owns checkbox/radio/pushbutton dispatch and `/NeedAppearances` handling. |
| Appearance generation: `generateAppearance(QPDFAnnotationObjectHelper&)` | `include/qpdf/QPDFFormFieldObjectHelper.hh:180-187`; dispatch definition `libqpdf/QPDFFormFieldObjectHelper.cc:472-480`, text rendering helper begins at `:766` | `generate_appearance(&mut AnnotationObjectHelper) -> Result<()>`; only `/Tx` and `/Ch` dispatch to the crate-private rendering primitives. |

## Boundary decisions

- Rust method names use snake case, but remain one-for-one group mappings to
  qpdf rather than compatibility wrappers in annotation or appearance modules.
- `Object`, `ObjectRef`, and `Option` express the object/null boundary where
  qpdf returns a `QPDFObjectHandle`; UTF-8-returning qpdf APIs become `String`.
- The pinned source's parent walks have cycle guards (`cc:39-46`, `cc:74-84`,
  `cc:120-130`). Rust must retain its malformed-chain cycle/depth protections.
