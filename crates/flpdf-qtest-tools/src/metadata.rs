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
    Ok((output, warnings))
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

/// Format qpdf's `test_parsedoffset` output and return recovery warnings separately.
pub fn format_parsed_offsets_with_diagnostics(path: &Path) -> Result<(String, String)> {
    let mut pdf = open(path)?;
    let warnings = repair_diagnostics(path, &pdf);
    let xref = pdf.get_xref_table();
    let objects = pdf.get_all_objects()?;
    let mut groups: Vec<Vec<ParsedObject>> = Vec::new();

    for object in objects {
        let object_ref = object.object_ref().ok_or_else(|| {
            Error::Internal("get_all_objects returned a direct object".to_owned())
        })?;
        let group = stream_group(object_ref, xref.get(&object_ref).copied())?;
        walk(&object, group, &mut groups);
    }

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
    output.push_str("succeeded\n");
    Ok((output, warnings))
}
