fn normalize_source(source: &str) -> String {
    source.replace("\r\n", "\n")
}

#[test]
fn security_restriction_helpers_do_not_expose_changed_bool() {
    let reader = normalize_source(include_str!("../src/reader.rs"));
    let acroform = normalize_source(include_str!("../src/acroform_document_helper.rs"));

    assert!(reader.contains("pub fn remove_security_restrictions(&mut self) -> Result<()>"));
    assert!(!reader.contains("pub fn remove_security_restrictions(&mut self) -> Result<bool>"));
    assert!(acroform.contains("pub fn disable_digital_signatures(&mut self) -> Result<()>"));
    assert!(!acroform.contains("pub fn disable_digital_signatures(&mut self) -> Result<bool>"));
    assert!(acroform.contains(
        "pub(crate) fn remove_form_fields(&mut self, to_remove: &BTreeSet<QpdfObjGen>) -> Result<()>"
    ));
    assert!(!acroform.contains(
        "pub(crate) fn remove_form_fields(&mut self, to_remove: &BTreeSet<QpdfObjGen>) -> Result<bool>"
    ));
    assert!(!reader.contains("qpdf-deviation-start: `changed` has no qpdf counterpart"));
    assert!(!acroform.contains("let mut changed = self.pdf.remove_security_restrictions()?"));
}
