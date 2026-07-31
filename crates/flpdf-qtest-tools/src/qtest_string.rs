pub(crate) fn utf8_value(stored: &[u8]) -> Vec<u8> {
    flpdf::pdf_string::utf8_value(stored)
}

pub(crate) fn new_unicode_string(utf8: &[u8]) -> Vec<u8> {
    flpdf::pdf_string::new_unicode_string(utf8)
}

pub(crate) fn unparse_binary(stored: &[u8]) -> Vec<u8> {
    flpdf::pdf_string::unparse_binary(stored)
}

#[cfg(test)]
mod tests {
    use super::{new_unicode_string, unparse_binary, utf8_value};

    #[test]
    fn delegates_pdf_string_operations_to_flpdf() {
        assert_eq!(utf8_value(&[0x80]), "•".as_bytes());
        assert_eq!(new_unicode_string(b"ASCII"), b"ASCII");
        assert_eq!(
            new_unicode_string("🥔".as_bytes()),
            b"\xfe\xff\xd8\x3e\xdd\x54"
        );
        assert_eq!(unparse_binary(b"A\n\x80"), b"<410a80>");
    }
}
