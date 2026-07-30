/// Return qpdf's argv-0 program name without a directory or Windows suffix.
pub fn program_name(argv0: &str) -> &str {
    let stem = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    stem.strip_suffix(".exe").unwrap_or(stem)
}

/// Return qpdf test_driver's argv-0 suffix after its last forward slash.
///
/// Unlike the compare helper, qpdf's driver preserves backslashes and `.exe`.
pub fn test_driver_program_name_bytes(argv0: &[u8]) -> &[u8] {
    argv0.rsplit(|byte| *byte == b'/').next().unwrap_or(argv0)
}

#[cfg(test)]
mod tests {
    use super::{program_name, test_driver_program_name_bytes};

    #[test]
    fn program_name_strips_unix_and_windows_paths_and_exe() {
        assert_eq!(program_name("/tmp/flpdf-test-driver"), "flpdf-test-driver");
        assert_eq!(
            program_name(r"C:\tmp\flpdf-test-driver.exe"),
            "flpdf-test-driver"
        );
    }

    #[test]
    fn program_name_preserves_a_bare_name() {
        assert_eq!(program_name("flpdf-test-compare"), "flpdf-test-compare");
    }

    #[test]
    fn test_driver_program_name_preserves_backslash_suffix_and_non_utf8() {
        assert_eq!(
            test_driver_program_name_bytes(b"/tmp/test-\xff\\driver.exe"),
            b"test-\xff\\driver.exe"
        );
    }
}
