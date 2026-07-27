//! qpdf correspondence: `ResourceFinder.cc`.
//!
//! Records the last name before resource-consuming content operators. This is
//! intentionally a direct `ParserCallbacks` consumer, rather than an
//! operation accumulator, to preserve qpdf's parser event semantics.

use std::collections::{BTreeMap, BTreeSet};

use crate::content_stream::{ParseControl, ParserCallbacks};
use crate::{Object, Result};

pub(crate) type ResourceNames = BTreeSet<Vec<u8>>;
pub(crate) type ResourceNamesByType = BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, BTreeSet<usize>>>;

#[derive(Debug, Default)]
pub(crate) struct ResourceFinder {
    last_name: Option<(Vec<u8>, usize)>,
    names: ResourceNames,
    names_by_resource_type: ResourceNamesByType,
    had_diagnostics: bool,
    pending_operands: bool,
    last_operator_started_at_boundary: bool,
}

impl ResourceFinder {
    pub(crate) fn names(&self) -> &ResourceNames {
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
    ) {
        self.names.insert(name.to_vec());
        self.names_by_resource_type
            .entry(resource_type.to_vec())
            .or_default()
            .entry(name.to_vec())
            .or_default()
            .insert(offset);
    }
}

fn resource_type_for_operator(operator: &[u8]) -> Option<&'static [u8]> {
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

impl ParserCallbacks for ResourceFinder {
    fn handle_object(
        &mut self,
        object: Object,
        offset: usize,
        _length: usize,
    ) -> Result<ParseControl> {
        match object {
            Object::Name(name) => {
                self.pending_operands = true;
                self.last_name = Some((name, offset));
            }
            Object::Operator(operator) => {
                self.last_operator_started_at_boundary = !self.pending_operands;
                self.pending_operands = false;
                if let (Some(resource_type), Some((name, name_offset))) = (
                    resource_type_for_operator(&operator),
                    self.last_name.clone(),
                ) {
                    self.record_resource_name(resource_type, &name, name_offset);
                }
            }
            Object::InlineImage(_) => {}
            _ => self.pending_operands = true,
        }
        Ok(ParseControl::Continue)
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
    use crate::content_stream::parse_content_stream_data;
    use crate::Result;

    fn find(input: &[u8]) -> Result<ResourceFinder> {
        let mut finder = ResourceFinder::default();
        parse_content_stream_data(input, &mut finder)?;
        Ok(finder)
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
        let finder = find(input).unwrap();
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

    fn run_qpdf_resource_finder_probe(probe: &Path, input: &[u8]) -> String {
        let output = Command::new(probe)
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
        String::from_utf8(output.stdout).expect("probe records are ASCII")
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
        assert_eq!(finder.names().len(), 10);
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
