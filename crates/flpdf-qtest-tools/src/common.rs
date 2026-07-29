/// Return qpdf's argv-0 program name without a directory or Windows suffix.
pub fn program_name(argv0: &str) -> &str {
    let stem = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    stem.strip_suffix(".exe").unwrap_or(stem)
}

#[cfg(test)]
mod tests {
    use super::program_name;

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
}
