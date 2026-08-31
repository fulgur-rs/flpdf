//! qpdf correspondence: `qpdf/test_renumber.cc` writer-renumber and
//! written-xref test helper.
//!
//! This is a qtest consumer. It exercises the canonical public `PdfWriter`
//! route and does not implement a second renumbering algorithm.

use flpdf::{Error, ObjectHandle, ObjectRef, ObjectStreamMode, Pdf, PdfWriter, Result, XrefEntry};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = concat!(
    "Usage: test_renumber [OPTION] INPUT.pdf\n",
    "Option:\n",
    "  --object-streams=preserve|disable|generate\n",
    "  --linearize\n",
    "  --preserve-unreferenced\n",
);

/// The qpdf helper's writer settings, with qpdf's defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenumberOptions {
    pub(crate) object_streams: ObjectStreamMode,
    pub(crate) linearize: bool,
    pub(crate) preserve_unreferenced: bool,
}

impl Default for RenumberOptions {
    fn default() -> Self {
        Self {
            object_streams: ObjectStreamMode::Preserve,
            linearize: false,
            preserve_unreferenced: false,
        }
    }
}

/// Run the helper using the supplied argv and output channels.
pub fn run(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> ExitCode {
    if args.len() < 2 {
        write_usage(stderr);
        return ExitCode::from(2);
    }

    let (options, input) = match parse_args(&args[1..]) {
        Ok(parsed) => parsed,
        Err(ParseError::Usage) => {
            write_usage(stderr);
            return ExitCode::from(2);
        }
    };

    match run_helper(&input, options, stdout, stderr) {
        Ok(()) => ExitCode::from(0),
        Err(error) => {
            write_error(stderr, &error);
            ExitCode::from(2)
        }
    }
}

enum ParseError {
    Usage,
}

fn parse_args(args: &[OsString]) -> std::result::Result<(RenumberOptions, PathBuf), ParseError> {
    let mut options = RenumberOptions::default();
    let mut input = None;

    for (index, arg) in args.iter().enumerate() {
        if let Some(option) = arg.to_str().filter(|value| value.starts_with('-')) {
            match option {
                "--object-streams=preserve" => {
                    options.object_streams = ObjectStreamMode::Preserve;
                }
                "--object-streams=disable" => {
                    options.object_streams = ObjectStreamMode::Disable;
                }
                "--object-streams=generate" => {
                    options.object_streams = ObjectStreamMode::Generate;
                }
                "--linearize" => options.linearize = true,
                "--preserve-unreferenced" => options.preserve_unreferenced = true,
                _ => return Err(ParseError::Usage),
            }
        } else if index + 1 != args.len() || input.is_some() {
            return Err(ParseError::Usage);
        } else {
            input = Some(PathBuf::from(arg));
        }
    }

    // qpdf leaves an empty filename when an option is supplied without a
    // positional input and lets processFile report that operation error. Keep
    // that behavior instead of turning it into a different usage branch.
    Ok((options, input.unwrap_or_default()))
}

fn run_helper(
    input: &PathBuf,
    options: RenumberOptions,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let input_file = File::open(input).map_err(|source| {
        let detail = source
            .to_string()
            .split_once(" (os error ")
            .map_or_else(|| source.to_string(), |(message, _)| message.to_owned());
        Error::System(format!("open {}: {detail}", input.display()))
    })?;
    let mut pdf = Pdf::open(input_file)?;
    let source_objects = pdf.get_all_objects()?;

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory()?;
    writer.set_object_stream_mode(options.object_streams);
    writer.set_linearization(options.linearize);
    writer.set_preserve_unreferenced_objects(options.preserve_unreferenced);
    writer.write()?;

    let mappings: Vec<_> = source_objects
        .iter()
        .map(|source| {
            let source_ref = source.object_ref().ok_or_else(|| {
                Error::Internal("test_renumber source object has no object reference".into())
            })?;
            Ok((
                source_ref,
                writer.get_renumbered_obj_gen(source_ref)?,
                source.clone(),
            ))
        })
        .collect::<Result<_>>()?;
    let written_xref = writer.get_written_xref_table()?;
    let output_bytes = writer.get_buffer()?;
    drop(writer);

    let mut reloaded = Pdf::open_mem_owned(output_bytes)?;
    let reloaded_xref = reloaded.get_xref_table();
    let mut visited = BTreeSet::new();

    writeln!(
        stdout,
        "--- compare between input and renumbered objects ---"
    )?;
    for (source_ref, renumbered, source) in mappings {
        let target_ref = renumbered.unwrap_or_else(|| ObjectRef::new(0, 0));
        writeln!(
            stdout,
            "input {}/{} -> renumbered {}/{}",
            source_ref.number, source_ref.generation, target_ref.number, target_ref.generation
        )?;
        if renumbered.is_none() {
            writeln!(stdout, "deleted")?;
            continue;
        }

        let target = reloaded.get_object_handle(target_ref);
        if !compare_objects(&source, &target, &mut visited, stdout, stderr)? {
            writeln!(stderr, "different")?;
            return Err(Error::Internal(
                "test_renumber object comparison failed".into(),
            ));
        }
    }
    writeln!(stdout, "complete")?;

    writeln!(
        stdout,
        "--- compare between written and reloaded xref tables ---"
    )?;
    if !compare_xref_tables(&written_xref, &reloaded_xref, stdout, stderr)? {
        writeln!(stderr, "different")?;
        return Err(Error::Internal(
            "test_renumber xref comparison failed".into(),
        ));
    }
    writeln!(stdout, "complete")?;
    writeln!(stdout, "succeeded")?;
    Ok(())
}

fn compare_objects(
    source: &ObjectHandle,
    emitted: &ObjectHandle,
    visited: &mut BTreeSet<ObjectRef>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<bool> {
    if let Some(object_ref) = source.object_ref() {
        if !visited.insert(object_ref) {
            return Ok(true);
        }
    }

    let source_type = source.type_code()?;
    let emitted_type = emitted.type_code()?;
    if source_type != emitted_type {
        writeln!(stderr, "different type code")?;
        return Ok(false);
    }

    match source_type {
        2 => Ok(true),
        3 => compare_value(source.as_boolean(), emitted.as_boolean(), "boolean", stderr),
        4 => compare_value(source.as_integer(), emitted.as_integer(), "integer", stderr),
        5 => compare_value(source.as_real(), emitted.as_real(), "real", stderr),
        6 => compare_value(source.as_string(), emitted.as_string(), "string", stderr),
        7 => compare_value(source.as_name(), emitted.as_name(), "name", stderr),
        8 => compare_arrays(source, emitted, visited, stdout, stderr),
        9 => compare_dictionaries(source, emitted, visited, stdout, stderr),
        10 => {
            writeln!(stdout, "stream objects are not compared")?;
            Ok(true)
        }
        _ => {
            writeln!(stderr, "unknown object type")?;
            Ok(false)
        }
    }
}

fn compare_value<T: PartialEq>(
    source: Option<T>,
    emitted: Option<T>,
    label: &str,
    stderr: &mut dyn Write,
) -> Result<bool> {
    if source == emitted {
        Ok(true)
    } else {
        writeln!(stderr, "different {label}")?;
        Ok(false)
    }
}

fn compare_arrays(
    source: &ObjectHandle,
    emitted: &ObjectHandle,
    visited: &mut BTreeSet<ObjectRef>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<bool> {
    let source_items = source.as_array().ok_or_else(|| {
        Error::Internal("test_renumber source array type has no array value".into())
    })?;
    let emitted_items = emitted.as_array().ok_or_else(|| {
        Error::Internal("test_renumber emitted array type has no array value".into())
    })?;
    if source_items.len() != emitted_items.len() {
        writeln!(stderr, "different array size")?;
        return Ok(false);
    }
    for (source_item, emitted_item) in source_items.iter().zip(emitted_items.iter()) {
        if !compare_objects(source_item, emitted_item, visited, stdout, stderr)? {
            writeln!(stderr, "different array item")?;
            return Ok(false);
        }
    }
    Ok(true)
}

fn compare_dictionaries(
    source: &ObjectHandle,
    emitted: &ObjectHandle,
    visited: &mut BTreeSet<ObjectRef>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<bool> {
    let source_entries = source.as_dictionary().ok_or_else(|| {
        Error::Internal("test_renumber source dictionary type has no dictionary value".into())
    })?;
    let emitted_entries = emitted.as_dictionary().ok_or_else(|| {
        Error::Internal("test_renumber emitted dictionary type has no dictionary value".into())
    })?;
    let source_keys: BTreeSet<_> = source_entries.keys().cloned().collect();
    let emitted_keys: BTreeSet<_> = emitted_entries.keys().cloned().collect();
    if source_keys != emitted_keys {
        writeln!(stderr, "different dictionary keys")?;
        return Ok(false);
    }
    for key in source_keys {
        let source_item = source.try_get_key(&key)?;
        let emitted_item = emitted.try_get_key(&key)?;
        if !compare_objects(&source_item, &emitted_item, visited, stdout, stderr)? {
            writeln!(stderr, "different dictionary item")?;
            return Ok(false);
        }
    }
    Ok(true)
}

fn compare_xref_tables(
    source: &BTreeMap<ObjectRef, XrefEntry>,
    emitted: &BTreeMap<ObjectRef, XrefEntry>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<bool> {
    if source.len() != emitted.len() {
        writeln!(stderr, "different size")?;
        return Ok(false);
    }

    for (&object_ref, &source_entry) in source {
        writeln!(
            stdout,
            "xref entry for {}/{}",
            object_ref.number, object_ref.generation
        )?;
        let Some(emitted_entry) = emitted.get(&object_ref) else {
            writeln!(stderr, "not found")?;
            return Ok(false);
        };
        if !same_xref_kind(source_entry, *emitted_entry) {
            writeln!(stderr, "different xref entry type")?;
            return Ok(false);
        }
    }
    Ok(true)
}

fn same_xref_kind(source: XrefEntry, emitted: XrefEntry) -> bool {
    matches!(
        (source, emitted),
        (XrefEntry::Free { .. }, XrefEntry::Free { .. })
            | (
                XrefEntry::Uncompressed { .. },
                XrefEntry::Uncompressed { .. }
            )
            | (XrefEntry::Compressed { .. }, XrefEntry::Compressed { .. })
    )
}

fn write_usage(stderr: &mut dyn Write) {
    let _ = stderr.write_all(USAGE.as_bytes());
}

fn write_error(stderr: &mut dyn Write, error: &Error) {
    let _ = writeln!(stderr, "{error}");
}

#[cfg(test)]
mod tests {
    use super::{compare_objects, compare_xref_tables, RenumberOptions};
    use flpdf::{ObjectHandle, ObjectRef, Pdf, XrefEntry};
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;

    #[test]
    fn options_default_to_qpdf_writer_defaults() {
        assert_eq!(
            RenumberOptions::default().object_streams,
            flpdf::ObjectStreamMode::Preserve
        );
        assert!(!RenumberOptions::default().linearize);
        assert!(!RenumberOptions::default().preserve_unreferenced);
    }

    #[test]
    fn object_comparison_reports_scalar_mismatch() {
        let source = ObjectHandle::integer(1);
        let emitted = ObjectHandle::integer(2);
        let mut visited = BTreeSet::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(
            !compare_objects(&source, &emitted, &mut visited, &mut stdout, &mut stderr,).unwrap()
        );
        assert_eq!(stderr, b"different integer\n");
    }

    #[test]
    fn object_comparison_reports_array_and_dictionary_mismatches() {
        let mut visited = BTreeSet::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(!compare_objects(
            &ObjectHandle::array(vec![ObjectHandle::integer(1)]),
            &ObjectHandle::array(vec![ObjectHandle::integer(1), ObjectHandle::integer(2)]),
            &mut visited,
            &mut stdout,
            &mut stderr,
        )
        .unwrap());
        assert_eq!(stderr, b"different array size\n");

        visited.clear();
        stderr.clear();
        assert!(!compare_objects(
            &ObjectHandle::dictionary(vec![(b"A".to_vec(), ObjectHandle::integer(1))]),
            &ObjectHandle::dictionary(vec![(b"B".to_vec(), ObjectHandle::integer(1))]),
            &mut visited,
            &mut stdout,
            &mut stderr,
        )
        .unwrap());
        assert_eq!(stderr, b"different dictionary keys\n");
    }

    #[test]
    fn object_comparison_skips_stream_payloads() {
        let source = ObjectHandle::stream(
            ObjectHandle::dictionary(Vec::new()),
            Rc::new(b"source".to_vec()),
        );
        let emitted = ObjectHandle::stream(
            ObjectHandle::dictionary(Vec::new()),
            Rc::new(b"emitted".to_vec()),
        );
        let mut visited = BTreeSet::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(
            compare_objects(&source, &emitted, &mut visited, &mut stdout, &mut stderr,).unwrap()
        );
        assert_eq!(stdout, b"stream objects are not compared\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn object_comparison_stops_at_an_indirect_cycle() {
        let mut pdf = Pdf::empty().unwrap();
        let cycle = pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        cycle.replace_key(b"/Self", cycle.clone()).unwrap();
        let mut visited = BTreeSet::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(compare_objects(&cycle, &cycle, &mut visited, &mut stdout, &mut stderr,).unwrap());
        assert!(stderr.is_empty());
    }

    #[test]
    fn xref_comparison_reports_missing_identity() {
        let mut source = BTreeMap::new();
        source.insert(ObjectRef::new(1, 0), XrefEntry::Uncompressed { offset: 10 });
        let emitted = BTreeMap::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(!compare_xref_tables(&source, &emitted, &mut stdout, &mut stderr,).unwrap());
        assert_eq!(stderr, b"different size\n");
    }

    #[test]
    fn xref_comparison_accepts_same_entry_kind_without_strengthening_qpdf_bug() {
        let object_ref = ObjectRef::new(1, 0);
        let source = BTreeMap::from([(object_ref, XrefEntry::Uncompressed { offset: 10 })]);
        let emitted = BTreeMap::from([(object_ref, XrefEntry::Uncompressed { offset: 20 })]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert!(compare_xref_tables(&source, &emitted, &mut stdout, &mut stderr,).unwrap());
        assert!(stderr.is_empty());
    }
}
