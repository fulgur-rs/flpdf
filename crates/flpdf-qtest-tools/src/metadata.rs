//! Thin qpdf helper consumers for source xref and parsed-offset metadata.
//!
//! The output contracts are ports of qpdf 11.9.0's `test_xref.cc` and
//! `test_parsedoffset.cc`. Parsing, xref construction, lazy resolution, and
//! provenance remain owned by [`flpdf`]; this module only walks the public
//! [`flpdf::Pdf`] / [`flpdf::ObjectHandle`] API and formats the result.
//!
//! Oracle sources: `qpdf/test_xref.cc:7-44` and
//! `qpdf/test_parsedoffset.cc:13-140` from the pinned qpdf 11.9.0 tree. The
//! initial file-open diagnostic follows qpdf's `QUtil::safe_fopen`
//! (`libqpdf/QUtil.cc:453-518`) and `QPDFSystemError::createWhat`
//! (`libqpdf/QPDFSystemError.cc:13-29`) by retaining the platform CRT text.

use flpdf::{
    Diagnostics, EncryptedError, Error, ObjectHandle, ObjectRef, Pdf, PdfOpenOptions, XrefEntry,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Failure from a metadata helper, retaining whether it happened during open
/// or after object enumeration had already begun.
#[derive(Debug)]
pub enum MetadataError {
    Flpdf(Error),
    Open {
        source: Error,
        crt_message: Option<Vec<u8>>,
    },
    PostEnumeration {
        source: Error,
        diagnostics: Diagnostics,
    },
}

/// Result type used by the metadata helper formatting boundary.
pub type Result<T> = std::result::Result<T, MetadataError>;

impl From<Error> for MetadataError {
    fn from(error: Error) -> Self {
        Self::Flpdf(error)
    }
}

fn open(path: &Path) -> Result<Pdf<std::fs::File>> {
    let file = std::fs::File::open(path).map_err(|source| MetadataError::Open {
        source: Error::FileIo {
            operation: "open",
            path: path.to_path_buf(),
            source,
        },
        crt_message: crate::driver::crt_open_error_message(path.as_os_str()),
    })?;
    Ok(Pdf::open_with_options(
        file,
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: crate::driver::os_str_diagnostic_bytes(path.as_os_str()).into_owned(),
            ..PdfOpenOptions::default()
        },
    )?)
}

/// Render an open/parse error in the shape emitted by qpdf's helper binaries.
pub fn display_error(path: &Path, error: &MetadataError) -> Vec<u8> {
    let mut message = Vec::new();
    match error {
        MetadataError::Flpdf(error) => display_flpdf_error(&mut message, path, error, None),
        MetadataError::Open {
            source,
            crt_message,
        } => display_flpdf_error(&mut message, path, source, crt_message.as_deref()),
        MetadataError::PostEnumeration {
            source,
            diagnostics,
        } => {
            for diagnostic in diagnostics.entries() {
                write_diagnostic(&mut message, path, diagnostic);
            }
            append_error_without_path(&mut message, source);
            if message.last() == Some(&b'\n') {
                message.pop();
            }
        }
    }
    message
}

fn display_flpdf_error(
    message: &mut Vec<u8>,
    path: &Path,
    error: &Error,
    crt_message: Option<&[u8]>,
) {
    match error {
        Error::FileIo {
            operation,
            path,
            source,
        } => {
            message.extend_from_slice(operation.as_bytes());
            message.push(b' ');
            append_path(message, path);
            message.extend_from_slice(b": ");
            if let Some(crt_message) = crt_message {
                message.extend_from_slice(crt_message);
            } else {
                let source_message = source
                    .to_string()
                    .split_once(" (os error ")
                    .map_or_else(|| source.to_string(), |(message, _)| message.to_owned());
                message.extend_from_slice(source_message.as_bytes());
            }
        }
        Error::OpenFailure {
            source,
            diagnostics,
        } => {
            for diagnostic in diagnostics.entries() {
                write_diagnostic(message, path, diagnostic);
            }
            append_error_with_path(message, path, source);
            if message.last() == Some(&b'\n') {
                message.pop();
            }
        }
        Error::Encrypted(encrypted) => {
            append_path(message, path);
            message.extend_from_slice(b": ");
            append_encrypted_detail(message, encrypted);
        }
        Error::Io(_) => {
            // qpdf's FileInputSource reports the operation and requested
            // initial read size, not Rust's `I/O error: ... (os error N)`.
            append_path(message, path);
            message.extend_from_slice(b": read 1024 bytes");
        }
        other => message.extend_from_slice(other.to_string().as_bytes()),
    }
}

fn append_path(output: &mut Vec<u8>, path: &Path) {
    #[cfg(unix)]
    {
        output.extend_from_slice(path.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    {
        output.extend_from_slice(path.to_string_lossy().as_bytes());
    }
}

fn append_error_with_path(output: &mut Vec<u8>, path: &Path, error: &Error) {
    append_path(output, path);
    output.extend_from_slice(b": ");
    match error {
        Error::Parse { message, .. } => output.extend_from_slice(message.as_bytes()),
        Error::Encrypted(encrypted) => append_encrypted_detail(output, encrypted),
        Error::Io(_) => output.extend_from_slice(b"read 1024 bytes"),
        Error::OpenFailure { source, .. } => append_error_without_path(output, source),
        other => output.extend_from_slice(other.to_string().as_bytes()),
    }
}

fn append_error_without_path(output: &mut Vec<u8>, error: &Error) {
    match error {
        Error::Parse { message, .. } => output.extend_from_slice(message.as_bytes()),
        Error::Encrypted(encrypted) => append_encrypted_detail(output, encrypted),
        Error::OpenFailure { source, .. } => append_error_without_path(output, source),
        other => output.extend_from_slice(other.to_string().as_bytes()),
    }
}

fn append_encrypted_detail(output: &mut Vec<u8>, error: &EncryptedError) {
    if matches!(error, EncryptedError::BadPassword) {
        output.extend_from_slice(b"invalid password");
    } else {
        output.extend_from_slice(error.to_string().as_bytes());
    }
}

fn write_diagnostic(output: &mut Vec<u8>, path: &Path, diagnostic: &flpdf::Diagnostic) {
    output.extend_from_slice(b"WARNING: ");
    append_path(output, path);
    if diagnostic.message.starts_with('(') {
        output.push(b' ');
    } else if let Some(offset) = diagnostic.offset.filter(|offset| *offset > 0) {
        output.extend_from_slice(format!(" (offset {offset}): ").as_bytes());
    } else {
        output.extend_from_slice(b": ");
    }
    output.extend_from_slice(diagnostic.message.as_bytes());
    output.push(b'\n');
}

fn repair_diagnostics<R: std::io::Read + std::io::Seek>(path: &Path, pdf: &Pdf<R>) -> Vec<u8> {
    let mut output = Vec::new();
    for diagnostic in pdf.repair_diagnostics().entries() {
        write_diagnostic(&mut output, path, diagnostic);
    }
    output
}

/// Format qpdf's `test_xref` output and return recovery warnings separately.
pub fn format_xref_with_diagnostics(path: &Path) -> Result<(String, Vec<u8>)> {
    let pdf = open(path)?;
    let mut output = String::new();
    for (object_ref, entry) in pdf.get_xref_table() {
        write_xref_entry(&mut output, object_ref, entry);
    }
    let warnings = repair_diagnostics(path, &pdf);
    Ok((output, warnings))
}

/// Write helper output without converting diagnostics or paths through UTF-8.
pub fn write_metadata_output(output: &str, warnings: &[u8]) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    stderr.write_all(warnings)?;
    stderr.flush()?;
    drop(stderr);

    let mut stdout = io::stdout().lock();
    stdout.write_all(output.as_bytes())?;
    stdout.flush()
}

fn write_xref_entry(output: &mut String, object_ref: ObjectRef, entry: XrefEntry) {
    write!(output, "{}/{}, ", object_ref.number, object_ref.generation)
        .expect("writing to String cannot fail");
    match entry {
        XrefEntry::Free { .. } => output.push_str("free entry\n"),
        XrefEntry::Uncompressed { offset } => {
            writeln!(output, "uncompressed, offset = {offset} (0x{offset:x})")
                .expect("writing to String cannot fail");
        }
        XrefEntry::Compressed { stream, index } => {
            writeln!(
                output,
                "compressed, stream number = {stream}, stream index = {index}"
            )
            .expect("writing to String cannot fail");
        }
    }
}

struct ParsedObject {
    offset: i64,
    description: String,
}

fn object_description(object: &ObjectHandle) -> std::result::Result<String, Error> {
    let offset = object.get_parsed_offset();
    let location = if let Some(ObjectRef { number, generation }) = object.object_ref() {
        format!("indirect {number}/{generation}")
    } else {
        "direct".to_owned()
    };
    let type_name = object.type_name()?;
    Ok(format!(
        "offset = {offset} (0x{offset:x}), {location}, {}",
        type_name
    ))
}

fn metadata_object_ref(object: &ObjectHandle) -> std::result::Result<ObjectRef, Error> {
    object
        .object_ref()
        .ok_or_else(|| Error::Internal("get_all_objects returned a direct object".to_owned()))
}

fn walk(
    object: &ObjectHandle,
    group: u32,
    groups: &mut BTreeMap<u32, Vec<ParsedObject>>,
) -> std::result::Result<(), Error> {
    groups.entry(group).or_default().push(ParsedObject {
        offset: object.get_parsed_offset(),
        description: object_description(object)?,
    });

    if let Some(items) = object.as_array() {
        for item in items {
            if !item.is_indirect() {
                walk(&item, group, groups)?;
            }
        }
    } else if let Some(entries) = object.as_dictionary() {
        for item in entries.into_values() {
            if !item.is_indirect() && !item.is_null() {
                walk(&item, group, groups)?;
            }
        }
    } else if let Some(dictionary) = object.as_stream_dict() {
        walk(&dictionary, group, groups)?;
    }
    Ok(())
}

fn stream_group(
    object_ref: ObjectRef,
    entry: Option<XrefEntry>,
) -> std::result::Result<u32, Error> {
    match entry {
        Some(XrefEntry::Uncompressed { .. }) => Ok(0),
        Some(XrefEntry::Compressed { stream, .. }) => Ok(stream),
        Some(XrefEntry::Free { .. }) => Err(Error::Internal(format!(
            "{}/{} xref entry is free",
            object_ref.number, object_ref.generation
        ))),
        None => Err(Error::Internal(format!(
            "{}/{} is not found in xref table",
            object_ref.number, object_ref.generation
        ))),
    }
}

fn render_parsed_groups(groups: &mut BTreeMap<u32, Vec<ParsedObject>>) -> String {
    let mut output = String::new();
    for (group, objects) in groups.iter_mut() {
        objects.sort_by(|left, right| {
            (left.offset, &left.description).cmp(&(right.offset, &right.description))
        });
        if *group == 0 {
            output.push_str("--- objects not in streams ---\n");
        } else {
            writeln!(output, "--- objects in stream {group} ---")
                .expect("writing to String cannot fail");
        }
        for object in objects {
            writeln!(output, "{}", object.description).expect("writing to String cannot fail");
        }
    }
    output
}

/// Format qpdf's `test_parsedoffset` output and return recovery warnings separately.
pub fn format_parsed_offsets_with_diagnostics(path: &Path) -> Result<(String, Vec<u8>)> {
    let mut pdf = open(path)?;
    let result: std::result::Result<String, Error> = (|| {
        let objects = pdf.get_all_objects()?;
        let xref = pdf.get_xref_table();
        let mut groups: BTreeMap<u32, Vec<ParsedObject>> = BTreeMap::new();

        for object in objects {
            let object_ref = metadata_object_ref(&object)?;
            let group = stream_group(object_ref, xref.get(&object_ref).copied())?;
            walk(&object, group, &mut groups)?;
        }

        let mut output = render_parsed_groups(&mut groups);
        output.push_str("succeeded\n");
        Ok(output)
    })();

    match result {
        Ok(output) => {
            let warnings = repair_diagnostics(path, &pdf);
            Ok((output, warnings))
        }
        Err(error) => {
            // qpdf keeps repair warnings on the live document until the
            // helper formats the terminal failure. Do the same here: a
            // post-enumeration error must not drop warnings collected while
            // reconstructing the effective xref table.
            let diagnostics = pdf.repair_diagnostics();
            if diagnostics.entries().is_empty() {
                Err(MetadataError::from(error))
            } else {
                Err(MetadataError::PostEnumeration {
                    source: error,
                    diagnostics,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::{Diagnostic, Diagnostics};

    #[test]
    fn diagnostics_match_qpdf_path_and_offset_formatting() {
        let path = Path::new("input.pdf");
        let mut output = Vec::new();
        write_diagnostic(
            &mut output,
            path,
            &Diagnostic::warning("(object 1 0, offset 7): object warning", None),
        );
        write_diagnostic(
            &mut output,
            path,
            &Diagnostic::warning("xref warning", Some(12)),
        );
        write_diagnostic(
            &mut output,
            path,
            &Diagnostic::warning("zero offset warning", Some(0)),
        );
        assert_eq!(
            output,
            b"WARNING: input.pdf (object 1 0, offset 7): object warning\n\
              WARNING: input.pdf (offset 12): xref warning\n\
              WARNING: input.pdf: zero offset warning\n"
        );

        let mut diagnostics = Diagnostics::default();
        diagnostics.push(Diagnostic::warning(
            "(object 1 0, offset 7): object warning",
            None,
        ));
        diagnostics.push(Diagnostic::warning("xref warning", Some(12)));
        let error = Error::OpenFailure {
            source: Box::new(Error::Internal("terminal failure".to_owned())),
            diagnostics,
        };
        assert_eq!(
            display_error(path, &MetadataError::from(error)),
            b"WARNING: input.pdf (object 1 0, offset 7): object warning\n\
              WARNING: input.pdf (offset 12): xref warning\n\
              input.pdf: terminal failure"
        );
    }

    #[test]
    fn bad_password_error_uses_qpdf_path_wording() {
        let error = Error::Encrypted(EncryptedError::BadPassword);
        assert_eq!(
            display_error(Path::new("secret.pdf"), &MetadataError::from(error),),
            b"secret.pdf: invalid password"
        );
    }

    #[test]
    fn every_encrypted_open_error_uses_the_input_path() {
        let error = Error::Encrypted(EncryptedError::UnsupportedHandler {
            filter: "Standard".to_owned(),
            v: 4,
            r: 4,
            cfm: Some("Unknown".to_owned()),
        });
        assert_eq!(
            display_error(
                Path::new("secret.pdf"),
                &MetadataError::from(error),
            ),
            b"secret.pdf: unsupported encryption handler: filter=Standard, V=4, R=4, CFM=Some(\"Unknown\")"
        );
    }

    #[test]
    fn read_errors_use_qpdf_file_input_wording() {
        let error = Error::Io(std::io::Error::from_raw_os_error(libc::EISDIR));
        assert_eq!(
            display_error(Path::new("directory.pdf"), &MetadataError::from(error),),
            b"directory.pdf: read 1024 bytes"
        );
    }

    #[test]
    fn open_errors_prefer_the_platform_crt_message() {
        let path = Path::new("missing.pdf");
        let error = MetadataError::Open {
            source: Error::FileIo {
                operation: "open",
                path: path.to_path_buf(),
                source: std::io::Error::other("Rust Win32 wording"),
            },
            crt_message: Some(b"The system cannot find the path specified.".to_vec()),
        };

        assert_eq!(
            display_error(path, &error),
            b"open missing.pdf: The system cannot find the path specified."
        );
    }

    #[test]
    fn stream_group_rejects_free_and_missing_entries() {
        let object_ref = ObjectRef::new(7, 2);
        assert_eq!(
            stream_group(object_ref, Some(XrefEntry::Uncompressed { offset: 9 }))
                .expect("uncompressed object group"),
            0
        );
        assert_eq!(
            stream_group(
                object_ref,
                Some(XrefEntry::Compressed {
                    stream: 11,
                    index: 3,
                }),
            )
            .expect("compressed object group"),
            11
        );

        let free_error = stream_group(object_ref, Some(XrefEntry::Free { next: 0 }))
            .expect_err("free object must not have a parsed offset group");
        assert_eq!(free_error.to_string(), "7/2 xref entry is free");

        let missing_error = stream_group(object_ref, None)
            .expect_err("missing object must not have a parsed offset group");
        assert_eq!(missing_error.to_string(), "7/2 is not found in xref table");
    }

    #[test]
    fn xref_formatter_includes_free_entries() {
        let mut output = String::new();
        write_xref_entry(
            &mut output,
            ObjectRef::new(7, 2),
            XrefEntry::Free { next: 0 },
        );
        assert_eq!(output, "7/2, free entry\n");
    }

    #[test]
    fn metadata_object_ref_rejects_direct_objects() {
        let error = metadata_object_ref(&ObjectHandle::integer(1))
            .expect_err("direct objects are not returned by get_all_objects");
        assert_eq!(
            error.to_string(),
            "get_all_objects returned a direct object"
        );
    }

    #[test]
    fn parsed_group_renderer_skips_empty_stream_slots() {
        let mut groups = BTreeMap::from([(
            2,
            vec![ParsedObject {
                offset: 4,
                description: "offset = 4 (0x4), indirect 7/0, integer".to_owned(),
            }],
        )]);
        assert_eq!(
            render_parsed_groups(&mut groups),
            "--- objects in stream 2 ---\n\
             offset = 4 (0x4), indirect 7/0, integer\n"
        );
    }

    #[test]
    fn dictionary_walk_skips_direct_null_values() {
        let object = ObjectHandle::dictionary(vec![
            (b"/Null".to_vec(), ObjectHandle::null()),
            (b"/Value".to_vec(), ObjectHandle::integer(7)),
        ]);
        let mut groups = BTreeMap::new();
        walk(&object, 0, &mut groups).expect("direct metadata walk succeeds");

        let descriptions: Vec<_> = groups[&0]
            .iter()
            .map(|object| object.description.as_str())
            .collect();
        assert_eq!(descriptions.len(), 2);
        assert!(descriptions
            .iter()
            .all(|description| !description.ends_with(", null")));
    }

    #[cfg(unix)]
    #[test]
    fn display_error_preserves_non_utf8_path_bytes() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let name = OsString::from_vec(b"missing-\xff.pdf".to_vec());
        let path = Path::new(name.as_os_str());
        let error = Error::FileIo {
            operation: "open",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(libc::ENOENT),
        };
        assert_eq!(
            display_error(path, &MetadataError::from(error)),
            b"open missing-\xff.pdf: No such file or directory"
        );
    }
}
