//! Public API contract for the qpdf-shaped form-field helper boundary.

use flpdf::form_field_object_helper::FormFieldObjectHelper;

#[test]
fn exposes_qpdf_form_field_helper_from_its_own_module() {
    let _ = std::any::type_name::<FormFieldObjectHelper<'static, std::io::Cursor<Vec<u8>>>>();
}
