//! qpdf correspondence: QPDF_String.cc UTF-8 value, Unicode-string construction, and forced binary unparsing exposed only to qtest helper binaries.
//! qpdf string operations exposed only to the qtest helper binaries.

/// Return qpdf's UTF-8 view of one stored PDF string.
pub fn utf8_value(stored: &[u8]) -> Vec<u8> {
    crate::json_inspect::qpdf_utf8_value(stored)
}

/// Construct the stored bytes for qpdf `newUnicodeString`.
pub fn new_unicode_string(utf8: &[u8]) -> Vec<u8> {
    crate::json_inspect::qpdf_unicode_string_bytes(utf8)
}

/// Force a stored PDF string into qpdf's binary hexadecimal representation.
pub fn unparse_binary(stored: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(stored.len().saturating_mul(2).saturating_add(2));
    crate::object::write_hex_string(&mut output, stored);
    output
}

#[cfg(test)]
mod tests {
    use super::{new_unicode_string, unparse_binary, utf8_value};

    #[test]
    fn pdfdoc_bullet_decodes_to_utf8() {
        assert_eq!(utf8_value(&[0x80]), "•".as_bytes());
    }

    #[test]
    fn ascii_unicode_string_stays_pdfdoc_encoded() {
        assert_eq!(new_unicode_string(b"ASCII"), b"ASCII");
    }

    #[test]
    fn potato_unicode_string_uses_utf16be() {
        assert_eq!(
            new_unicode_string("🥔".as_bytes()),
            b"\xfe\xff\xd8\x3e\xdd\x54"
        );
    }

    #[test]
    fn pdfdoc_bom_prefix_is_forced_to_utf16be() {
        assert_eq!(
            new_unicode_string("þÿ".as_bytes()),
            b"\xfe\xff\x00\xfe\x00\xff"
        );
        assert_eq!(
            new_unicode_string("ÿþ".as_bytes()),
            b"\xfe\xff\x00\xff\x00\xfe"
        );
        assert_eq!(
            new_unicode_string("ï»¿".as_bytes()),
            b"\xfe\xff\x00\xef\x00\xbb\x00\xbf"
        );
    }

    #[test]
    fn malformed_utf8_is_replaced_before_utf16be_encoding() {
        assert_eq!(
            new_unicode_string(b"\xfeafter"),
            b"\xfe\xff\xff\xfd\x00a\x00f\x00t\x00e\x00r"
        );
    }

    #[test]
    fn binary_unparse_is_lowercase_hex() {
        assert_eq!(unparse_binary(b"A\n\x80"), b"<410a80>");
    }
}
