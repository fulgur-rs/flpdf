//! Thin qpdf helper consumers for source xref and parsed-offset metadata.
//!
//! The output contracts are ports of qpdf 11.9.0's `test_xref.cc` and
//! `test_parsedoffset.cc`. Parsing, xref construction, lazy resolution, and
//! provenance remain owned by [`flpdf`]; this module only walks the public
//! [`flpdf::Pdf`] / [`flpdf::ObjectHandle`] API and formats the result.
//!
//! Oracle sources: `qpdf/test_xref.cc:7-44` and
//! `qpdf/test_parsedoffset.cc:13-140` from the pinned qpdf 11.9.0 tree.

use flpdf::{Error, ObjectHandle, ObjectRef, Pdf, PdfOpenOptions, Result, XrefEntry};
use std::fmt::Write as _;
use std::io::Cursor;
use std::path::Path;

fn open(path: &Path) -> Result<Pdf<Cursor<Vec<u8>>>> {
    let bytes = std::fs::read(path).map_err(|source| Error::FileIo {
        operation: "open",
        path: path.to_path_buf(),
        source,
    })?;
    Pdf::open_mem_owned_with_options(
        bytes,
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            description: path.display().to_string(),
            ..PdfOpenOptions::default()
        },
    )
}

/// Render an open/parse error in the shape emitted by qpdf's helper binaries.
pub fn display_error(path: &Path, error: &Error) -> String {
    match error {
        Error::FileIo {
            operation,
            path,
            source,
        } => {
            let message = source
                .to_string()
                .split_once(" (os error ")
                .map_or_else(|| source.to_string(), |(message, _)| message.to_owned());
            format!("{operation} {}: {message}", path.display())
        }
        Error::OpenFailure {
            source,
            diagnostics,
        } => {
            let mut message = String::new();
            for diagnostic in diagnostics.entries() {
                write_diagnostic(&mut message, path, diagnostic);
            }
            match source.as_ref() {
                Error::Parse { message: error, .. } => {
                    writeln!(message, "{}: {error}", path.display())
                        .expect("writing to String cannot fail");
                }
                other => {
                    writeln!(message, "{}: {}", path.display(), other)
                        .expect("writing to String cannot fail");
                }
            }
            message.pop();
            message
        }
        other => other.to_string(),
    }
}

fn write_diagnostic(output: &mut String, path: &Path, diagnostic: &flpdf::Diagnostic) {
    output.push_str("WARNING: ");
    output.push_str(&path.display().to_string());
    if diagnostic.message.starts_with('(') {
        output.push(' ');
    } else if let Some(offset) = diagnostic.offset {
        write!(output, " (offset {offset}): ").expect("writing to String cannot fail");
    } else {
        output.push_str(": ");
    }
    output.push_str(&diagnostic.message);
    output.push('\n');
}

fn repair_diagnostics<R: std::io::Read + std::io::Seek>(path: &Path, pdf: &Pdf<R>) -> String {
    let mut output = String::new();
    for diagnostic in pdf.repair_diagnostics().entries() {
        write_diagnostic(&mut output, path, diagnostic);
    }
    output
}

/// Format qpdf's `test_xref` output and return recovery warnings separately.
pub fn format_xref_with_diagnostics(path: &Path) -> Result<(String, String)> {
    let pdf = open(path)?;
    let warnings = repair_diagnostics(path, &pdf);
    let mut output = String::new();
    for (object_ref, entry) in pdf.get_xref_table() {
        write_xref_entry(&mut output, object_ref, entry);
    }
    Ok((output, warnings))
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

fn object_description(object: &ObjectHandle) -> String {
    let offset = object.get_parsed_offset();
    let location = if let Some(ObjectRef { number, generation }) = object.object_ref() {
        format!("indirect {number}/{generation}")
    } else {
        "direct".to_owned()
    };
    format!(
        "offset = {offset} (0x{offset:x}), {location}, {}",
        object.type_name()
    )
}

fn metadata_object_ref(object: &ObjectHandle) -> Result<ObjectRef> {
    object
        .object_ref()
        .ok_or_else(|| Error::Internal("get_all_objects returned a direct object".to_owned()))
}

fn walk(object: &ObjectHandle, group: usize, groups: &mut Vec<Vec<ParsedObject>>) {
    if groups.len() <= group {
        groups.resize_with(group + 1, Vec::new);
    }
    groups[group].push(ParsedObject {
        offset: object.get_parsed_offset(),
        description: object_description(object),
    });

    if let Some(items) = object.as_array() {
        for item in items {
            if !item.is_indirect() {
                walk(&item, group, groups);
            }
        }
    } else if let Some(entries) = object.as_dictionary() {
        for item in entries.into_values() {
            if !item.is_indirect() {
                walk(&item, group, groups);
            }
        }
    } else if let Some(dictionary) = object.as_stream_dict() {
        walk(&dictionary, group, groups);
    }
}

fn stream_group(object_ref: ObjectRef, entry: Option<XrefEntry>) -> Result<usize> {
    match entry {
        Some(XrefEntry::Uncompressed { .. }) => Ok(0),
        Some(XrefEntry::Compressed { stream, .. }) => Ok(stream as usize),
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

fn render_parsed_groups(groups: &mut [Vec<ParsedObject>]) -> String {
    let mut output = String::new();
    for (group, objects) in groups.iter_mut().enumerate() {
        if objects.is_empty() {
            continue;
        }
        objects.sort_by(|left, right| {
            (left.offset, &left.description).cmp(&(right.offset, &right.description))
        });
        if group == 0 {
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
pub fn format_parsed_offsets_with_diagnostics(path: &Path) -> Result<(String, String)> {
    let mut pdf = open(path)?;
    let warnings = repair_diagnostics(path, &pdf);
    let xref = pdf.get_xref_table();
    let objects = pdf.get_all_objects()?;
    let mut groups: Vec<Vec<ParsedObject>> = Vec::new();

    for object in objects {
        let object_ref = metadata_object_ref(&object)?;
        let group = stream_group(object_ref, xref.get(&object_ref).copied())?;
        walk(&object, group, &mut groups);
    }

    let mut output = render_parsed_groups(&mut groups);
    output.push_str("succeeded\n");
    Ok((output, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::{Diagnostic, Diagnostics};

    #[test]
    fn diagnostics_match_qpdf_path_and_offset_formatting() {
        let path = Path::new("input.pdf");
        let mut output = String::new();
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
        assert_eq!(
            output,
            "WARNING: input.pdf (object 1 0, offset 7): object warning\n\
             WARNING: input.pdf (offset 12): xref warning\n"
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
            display_error(path, &error),
            "WARNING: input.pdf (object 1 0, offset 7): object warning\n\
             WARNING: input.pdf (offset 12): xref warning\n\
             input.pdf: terminal failure"
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
        let mut groups = vec![
            Vec::new(),
            Vec::new(),
            vec![ParsedObject {
                offset: 4,
                description: "offset = 4 (0x4), indirect 7/0, integer".to_owned(),
            }],
        ];
        assert_eq!(
            render_parsed_groups(&mut groups),
            "--- objects in stream 2 ---\n\
             offset = 4 (0x4), indirect 7/0, integer\n"
        );
    }
}
