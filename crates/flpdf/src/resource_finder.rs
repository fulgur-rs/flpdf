//! qpdf correspondence: `ResourceFinder.cc`.
//!
//! Records the last name before resource-consuming content operators. This is
//! intentionally a direct `ObjectHandleParserCallbacks` consumer, rather than
//! an operation accumulator, to preserve qpdf's parser event semantics
//! (`libqpdf/ResourceFinder.cc:3-56`).

use std::collections::{BTreeMap, BTreeSet};

use crate::content_stream::{ObjectHandleParserCallbacks, ParseControl};
use crate::object_handle::ObjectHandle;
use crate::Result;

pub(crate) type ResourceNamesByType = BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, BTreeSet<usize>>>;

#[derive(Debug, Default)]
pub(crate) struct ResourceFinder {
    last_name: Option<(Vec<u8>, usize)>,
    names: BTreeSet<Vec<u8>>,
    names_by_resource_type: ResourceNamesByType,
    had_diagnostics: bool,
    pending_operands: bool,
    last_operator_started_at_boundary: bool,
}

impl ResourceFinder {
    /// Return qpdf `ResourceFinder::getNames()` semantics: one flat set of
    /// names referenced by any resource-consuming operator, regardless of
    /// resource category.
    pub(crate) fn names(&self) -> &BTreeSet<Vec<u8>> {
        &self.names
    }

    pub(crate) fn names_by_resource_type(&self) -> &ResourceNamesByType {
        &self.names_by_resource_type
    }

    pub(crate) fn had_diagnostics(&self) -> bool {
        self.had_diagnostics
    }

    pub(crate) fn last_operator_started_at_boundary(&self) -> bool {
        self.last_operator_started_at_boundary
    }

    pub(crate) fn has_pending_operands(&self) -> bool {
        self.pending_operands
    }

    pub(crate) fn record_resource_name(
        &mut self,
        resource_type: &[u8],
        name: &[u8],
        offset: usize,
    ) -> bool {
        let inserted = Self::insert_resource_name(
            &mut self.names_by_resource_type,
            resource_type,
            name,
            offset,
        );
        self.names.insert(name.to_vec());
        inserted
    }

    fn insert_resource_name(
        names_by_resource_type: &mut ResourceNamesByType,
        resource_type: &[u8],
        name: &[u8],
        offset: usize,
    ) -> bool {
        if names_by_resource_type
            .get(resource_type)
            .and_then(|names| names.get(name))
            .is_some_and(|offsets| offsets.contains(&offset))
        {
            return false;
        }

        names_by_resource_type
            .entry(resource_type.to_vec())
            .or_default()
            .entry(name.to_vec())
            .or_default()
            .insert(offset)
    }

    fn record_last_name(&mut self, resource_type: &[u8]) {
        let Some((name, offset)) = self.last_name.as_ref() else {
            return;
        };
        let name = name.clone();
        let offset = *offset;
        Self::insert_resource_name(
            &mut self.names_by_resource_type,
            resource_type,
            &name,
            offset,
        );
        self.names.insert(name);
    }

    /// Canonical ObjectHandle-native callback used by page and Form resource
    /// pruning. Parsed content never crosses the legacy `Object` boundary.
    pub(crate) fn handle_object_handle(
        &mut self,
        object: &ObjectHandle,
        offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        if let Some(name) = object.as_name() {
            self.pending_operands = true;
            self.last_name = Some((name, offset));
        } else if let Some(operator) = object.as_operator() {
            self.last_operator_started_at_boundary = !self.pending_operands;
            self.pending_operands = false;
            if let Some(resource_type) = operator_resource_type(&operator) {
                self.record_last_name(resource_type);
            }
        } else if object.as_inline_image().is_some() {
            // Inline-image payloads carry no resource operand semantics here;
            // their `/CS` header is handled by the dedicated content scanner.
        } else {
            self.pending_operands = true;
        }
        Ok(ParseControl::Continue)
    }
}

fn operator_resource_type(operator: &[u8]) -> Option<&'static [u8]> {
    match operator {
        b"CS" | b"cs" => Some(b"ColorSpace"),
        b"gs" => Some(b"ExtGState"),
        b"Tf" => Some(b"Font"),
        b"SCN" | b"scn" => Some(b"Pattern"),
        b"BDC" | b"DP" => Some(b"Properties"),
        b"sh" => Some(b"Shading"),
        b"Do" => Some(b"XObject"),
        _ => None,
    }
}

impl ObjectHandleParserCallbacks for ResourceFinder {
    fn handle_object(
        &mut self,
        object: ObjectHandle,
        offset: usize,
        length: usize,
    ) -> Result<ParseControl> {
        self.handle_object_handle(&object, offset, length)
    }

    fn handle_diagnostic(&mut self, _offset: usize, _message: &str) -> Result<()> {
        self.had_diagnostics = true;
        Ok(())
    }

    fn handle_eof(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Write;
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::content_stream::parse_content_stream_handles;
    use crate::Result;

    fn find(input: &[u8]) -> Result<ResourceFinder> {
        let mut finder = ResourceFinder::default();
        parse_content_stream_handles(input, None, &mut finder)?;
        Ok(finder)
    }

    #[test]
    fn names_are_flat_across_resource_types() {
        let finder = find(b"/Shared 12 Tf /Shared Do").expect("content should parse");

        assert_eq!(finder.names().len(), 1);
        assert!(finder.names().contains(b"Shared".as_slice()));
        assert!(
            finder.names_by_resource_type()[b"Font".as_slice()].contains_key(b"Shared".as_slice())
        );
        assert!(finder.names_by_resource_type()[b"XObject".as_slice()]
            .contains_key(b"Shared".as_slice()));
    }

    #[test]
    fn canonical_callbacks_cover_inline_image_and_diagnostic_events() {
        let mut finder = ResourceFinder::default();
        let inline = ObjectHandle::inline_image(b"payload".to_vec());

        assert_eq!(
            finder
                .handle_object_handle(&inline, 0, inline.as_inline_image().unwrap().len())
                .unwrap(),
            ParseControl::Continue
        );
        ObjectHandleParserCallbacks::handle_diagnostic(&mut finder, 2, "recovered")
            .expect("diagnostics are warning-only");

        assert!(finder.had_diagnostics());
        assert!(!finder.has_pending_operands());
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn qpdf_name_hex(name: &[u8]) -> String {
        let mut canonical = Vec::with_capacity(name.len() + 1);
        canonical.push(b'/');
        canonical.extend_from_slice(name);
        hex_encode(&canonical)
    }

    fn dump_flpdf_resource_finder(input: &[u8]) -> String {
        let mut finder = ResourceFinder::default();
        parse_content_stream_handles(input, None, &mut finder).unwrap();
        let mut records = String::new();
        for name in finder.names() {
            writeln!(records, "name\t{}", qpdf_name_hex(name)).unwrap();
        }
        for (resource_type, names) in finder.names_by_resource_type() {
            for (name, offsets) in names {
                for offset in offsets {
                    writeln!(
                        records,
                        "resource\t{}\t{}\t{offset}",
                        qpdf_name_hex(resource_type),
                        qpdf_name_hex(name),
                    )
                    .unwrap();
                }
            }
        }
        records
    }

    fn run_qpdf_resource_finder_probe_command(
        mut command: Command,
        probe: &Path,
        input: &[u8],
    ) -> String {
        let output = command
            .args([
                "--mode",
                "resource-finder",
                "--input-hex",
                &hex_encode(input),
                "--allow-eof",
                "1",
                "--include-ignorable",
                "0",
                "--allow-bad",
                "1",
                "--max-len",
                "0",
                "--inline-offset",
                "none",
                "--chunks",
                "all",
            ])
            .output()
            .unwrap_or_else(|error| {
                panic!(
                    "failed to execute qpdf resource finder probe {}: {error}",
                    probe.display()
                )
            });
        assert!(
            output.status.success(),
            "qpdf resource finder probe failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).expect("probe records are ASCII") // cov:ignore: the pinned qpdf probe emits ASCII records by contract
    }

    fn run_qpdf_resource_finder_probe(probe: &Path, input: &[u8]) -> String {
        run_qpdf_resource_finder_probe_command(Command::new(probe), probe, input)
    }

    /// Write a stand-in probe script.
    ///
    /// The script is handed to `/bin/sh` as an argument rather than executed
    /// directly, so a still-open write handle cannot make the spawn fail with
    /// `ETXTBSY`.
    #[cfg(unix)]
    fn write_test_probe(path: &Path, source: &str) {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, source).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    fn run_test_probe(probe: &Path, input: &[u8]) -> String {
        let mut command = Command::new("/bin/sh");
        command.arg(probe);
        run_qpdf_resource_finder_probe_command(command, probe, input)
    }

    #[cfg(unix)]
    #[test]
    fn resource_finder_probe_passes_exact_arguments_and_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(
            &probe,
            "#!/bin/sh\nprintf '%s' \"$1\"\nshift\nprintf ' %s' \"$@\"\nprintf '\\n'\n",
        );
        assert_eq!(
            run_test_probe(&probe, b"/F1 12 Tf"),
            "--mode resource-finder --input-hex 2f4631203132205466 --allow-eof 1 \
             --include-ignorable 0 --allow-bad 1 --max-len 0 --inline-offset none \
             --chunks all\n"
        );
        assert_eq!(
            dump_flpdf_resource_finder(b"/F1 12 Tf"),
            "name\t2f4631\nresource\t2f466f6e74\t2f4631\t0\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resource_finder_probe_that_is_still_open_for_writing_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(&probe, "#!/bin/sh\nprintf 'name\\t2f4631\\n'\n");
        let _write_open = std::fs::OpenOptions::new()
            .write(true)
            .open(&probe)
            .unwrap();

        assert_eq!(run_test_probe(&probe, b"/F1 12 Tf"), "name\t2f4631\n");
    }

    #[cfg(unix)]
    #[test]
    fn resource_finder_probe_spawn_failure_reports_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join(OsStr::from_bytes(b"missing-\xff-probe"));

        let panic =
            std::panic::catch_unwind(|| run_qpdf_resource_finder_probe(&probe, b"/F1 12 Tf"))
                .unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("failed to execute qpdf resource finder probe"));
        assert!(message.contains(&probe.display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn resource_finder_probe_failure_reports_status_and_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        write_test_probe(
            &probe,
            "#!/bin/sh\nprintf 'resource finder probe stderr' >&2\nexit 1\n",
        );

        let panic = std::panic::catch_unwind(|| run_test_probe(&probe, b"/F1 12 Tf")).unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("qpdf resource finder probe failed (exit status: 1)"));
        assert!(message.contains("resource finder probe stderr"));
    }

    #[test]
    fn records_qpdf_operator_table_with_raw_name_offsets() {
        let input = b"/CS1 CS /cs1 cs /GS1 gs /F1 12 Tf /P1 SCN /p1 scn \
                      /Span /MC1 BDC /Span /MC2 DP /Sh1 sh /X1 Do";
        let finder = find(input).unwrap();
        assert_eq!(
            finder.names_by_resource_type()[b"Font".as_slice()][b"F1".as_slice()],
            BTreeSet::from([input.windows(3).position(|w| w == b"/F1").unwrap()])
        );
        assert_eq!(
            finder.names_by_resource_type()[b"XObject".as_slice()]
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![b"X1".to_vec()]
        );
        let flat_names = finder
            .names_by_resource_type()
            .values()
            .flat_map(|by_name| by_name.keys())
            .collect::<BTreeSet<_>>();
        assert_eq!(flat_names.len(), 10);
    }

    #[test]
    fn last_name_survives_non_name_operands_and_resource_operators() {
        let finder = find(b"/F1 12 Tf 99 Tf").unwrap();
        assert_eq!(
            finder.names_by_resource_type()[b"Font".as_slice()][b"F1".as_slice()].len(),
            1
        );
    }

    #[test]
    fn duplicate_resource_record_reports_no_insertion() {
        let mut finder = ResourceFinder::default();

        assert!(finder.record_resource_name(b"XObject", b"VeryLongFormName", 0));
        assert!(!finder.record_resource_name(b"XObject", b"VeryLongFormName", 0));
    }

    #[test]
    fn final_name_before_bdc_and_dp_is_the_properties_name() {
        let finder = find(b"/Span /MC1 BDC /Tag /MC2 DP").unwrap();
        assert!(finder.names_by_resource_type()[b"Properties".as_slice()]
            .contains_key(b"MC1".as_slice()));
        assert!(finder.names_by_resource_type()[b"Properties".as_slice()]
            .contains_key(b"MC2".as_slice()));
    }

    #[test]
    fn parser_diagnostics_mark_results_incomplete() {
        let finder = find(b"<0g> /F1 12 Tf").unwrap();
        assert!(finder.had_diagnostics());
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 ResourceFinder oracle"]
    // cov:ignore-start: the ignored live oracle is separately run by qpdf-tokenizer-diff.sh
    fn qpdf_resource_finder_differential() {
        let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
            .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
        for (name, input) in [
            (
                "all-operators-repeated-escaped-and-comments",
                b"% leading comment\n/CS#31 CS /cs1 cs /GS1 gs /F1 12 Tf /P1 SCN /p1 scn \
                  /Span /MC1 BDC /Tag /MC2 DP /Sh1 sh /X1 Do /F1 9 Tf"
                    .as_slice(),
            ),
            ("malformed-content", b"<0g> /F1 12 Tf".as_slice()),
            (
                "inline-image",
                b"/F1 12 Tf BI /W 1 ID \x00x EI /X1 Do".as_slice(),
            ),
            (
                "incomplete-inline-image-keeps-prefix",
                b"/F1 12 Tf BI ID".as_slice(),
            ),
        ] {
            assert_eq!(
                dump_flpdf_resource_finder(input),
                run_qpdf_resource_finder_probe(Path::new(&probe), input),
                "resource finder case {name}",
            );
        }
    }
    // cov:ignore-end
}
