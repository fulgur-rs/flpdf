//! qpdf correspondence: shared QPDFFileSpecObjectHelper/QPDFEFStreamObjectHelper support primitives.

use crate::pipeline::md5::PlMd5;
use crate::pipeline::{Discard, Pipeline};
use crate::{Error, ObjectHandle, Pdf, Result};
use std::io::{Read, Seek};
use std::path::Path;

pub(super) const NAME_KEYS: [&str; 5] = ["UF", "F", "Unix", "DOS", "Mac"];

pub(super) fn ensure_indirect_handle_belongs_to_pdf<R: Read + Seek>(
    handle: &ObjectHandle,
    pdf: &mut Pdf<R>,
    kind: &str,
) -> Result<()> {
    if handle.belongs_to_pdf(pdf.unique_id()) {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "{kind} handle belongs to another Pdf"
        )))
    }
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

// qpdf's `QUtil::safe_fopen` reports `"open " + filename + ": " +
// strerror(errno)` (`libqpdf/QUtil.cc:512-515`, `QPDFSystemError.cc:12-27`),
// with no numeric error code. `std::io::Error`'s `Display` appends a
// `" (os error N)"` suffix that qpdf's message lacks; strip it so the two
// diagnostics match byte-for-byte. A missing file is special-cased to
// qpdf's portable C-runtime wording ("No such file or directory") on every
// host, since Rust's `std::io::Error` on Windows instead surfaces the
// native Win32 FormatMessage text ("The system cannot find the file
// specified."). Keep this diagnostic helper available to the JSON input
// path without coupling that path to either qpdf-shaped object helper.
// The byte-carrying error keeps the path intact for the CLI renderer; ordinary
// Display callers still receive the lossy projection.
pub(crate) fn qpdf_style_open_error(path: &Path, error: std::io::Error) -> Error {
    let rendered = error.to_string();
    let message = if error.kind() == std::io::ErrorKind::NotFound {
        "No such file or directory"
    } else {
        error
            .raw_os_error()
            .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
            .unwrap_or(&rendered)
    };
    let mut raw_message = b"open ".to_vec();
    raw_message.extend_from_slice(&path_bytes(path));
    raw_message.extend_from_slice(b": ");
    raw_message.extend_from_slice(message.as_bytes());
    Error::SystemBytes(raw_message)
}

/// Encode a Unicode filename as UTF-16BE with a BOM.
pub fn encode_utf16be(s: &str) -> Vec<u8> {
    let mut out = vec![0xFE_u8, 0xFF];
    for unit in s.encode_utf16() {
        out.push((unit >> 8) as u8);
        out.push((unit & 0xFF) as u8);
    }
    out
}

/// Format a UTC PDF date string without validating its component fields.
pub fn format_pdf_date(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Vec<u8> {
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z").into_bytes()
}

/// Compute the binary MD5 checksum used by `/Params /CheckSum`.
pub fn md5_checksum(data: &[u8]) -> Vec<u8> {
    let mut discard = Discard;
    let mut md5 = PlMd5::new("EF md5", &mut discard);
    md5.write(data)
        .expect("embedded-file MD5 discard write is infallible");
    md5.finish()
        .expect("embedded-file MD5 discard finish is infallible");
    let hex_digest = md5
        .get_hex_digest()
        .expect("embedded-file MD5 pipeline remains enabled");
    hex::decode(hex_digest).expect("PlMd5 always returns lowercase hexadecimal")
}
