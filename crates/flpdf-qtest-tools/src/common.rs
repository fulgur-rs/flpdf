use flpdf::{Error, ObjectHandle, ObjectRef, PageDocumentHelper, Pdf};
use std::io::{Read, Seek};

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

/// Project the canonical raw page list at qtest-driver consumers whose output
/// or following operation explicitly uses valid `N G R` references.
pub(crate) fn checked_page_refs<R: Read + Seek>(pdf: &mut Pdf<R>) -> flpdf::Result<Vec<ObjectRef>> {
    raw_page_handles(pdf)?
        .into_iter()
        .map(|page| {
            let object_gen = page.get_obj_gen();
            page.object_ref().ok_or_else(|| {
                Error::Unsupported(format!(
                    "qtest page {} {} is not a valid N G R reference",
                    object_gen.get_obj(),
                    object_gen.get_gen()
                ))
            })
        })
        .collect()
}

/// Enumerate qpdf's repaired page list without discarding raw object identity.
pub(crate) fn raw_page_handles<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> flpdf::Result<Vec<ObjectHandle>> {
    PageDocumentHelper::new(pdf).get_all_pages()
}

/// Count qpdf's repaired raw page list without requiring indirect references.
#[cfg(test)]
pub(crate) fn raw_page_count<R: Read + Seek>(pdf: &mut Pdf<R>) -> flpdf::Result<usize> {
    Ok(raw_page_handles(pdf)?.len())
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
