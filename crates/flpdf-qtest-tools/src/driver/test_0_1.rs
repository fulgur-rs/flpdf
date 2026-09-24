use std::io::{Read, Seek, Write};
use std::sync::{Arc, Mutex};

use flpdf::{
    DecodeLevel, Error, ObjectHandle, ObjectRef, Pdf, Pipeline, PipelineError, PipelineHandle,
    PipelineResult, QPDFLogger,
};

use super::emit_new_diagnostics;
use super::handle::{resolve_handle, write_qpdf_object_handle};
use crate::output::write_bytes;

#[derive(Clone, Debug)]
enum OrderedStreamEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

type OrderedStreamEvents = Arc<Mutex<Vec<OrderedStreamEvent>>>;

fn record_stream_event(
    events: &OrderedStreamEvents,
    event: OrderedStreamEvent,
) -> PipelineResult<()> {
    events
        .lock()
        .map_err(|_| PipelineError::runtime("ordered stream event mutex poisoned"))?
        .push(event);
    Ok(())
}

struct OrderedStreamOutput {
    events: OrderedStreamEvents,
}

impl Pipeline for OrderedStreamOutput {
    fn identifier(&self) -> &str {
        "qtest stream output"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        record_stream_event(&self.events, OrderedStreamEvent::Stdout(data.to_vec()))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

struct OrderedStreamWarning {
    events: OrderedStreamEvents,
}

impl Pipeline for OrderedStreamWarning {
    fn identifier(&self) -> &str {
        "qtest stream warnings"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        record_stream_event(&self.events, OrderedStreamEvent::Stderr(data.to_vec()))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn replay_stream_events(
    events: &OrderedStreamEvents,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<()> {
    let events = events
        .lock()
        .map_err(|_| Error::System("ordered stream event mutex poisoned".to_owned()))?
        .clone();
    stdout.flush()?;
    for event in events {
        match event {
            OrderedStreamEvent::Stdout(data) => stdout.write_all(&data)?,
            OrderedStreamEvent::Stderr(data) => {
                stdout.flush()?;
                stderr.write_all(&data)?;
            }
        }
    }
    stdout.flush()?;
    Ok(())
}

pub(crate) fn run_test_0_1<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // `Pdf::trailer_key_handle`, not `Pdf::trailer().get_key(...)`:
    // the latter lifts the *entire* trailer in one structural walk bounded
    // by the crate's inline-object-nesting limit, so an unrelated, deeply
    // nested sibling trailer entry would degrade `/QTest` to null here too,
    // even though `/QTest` itself is untouched.
    let original = pdf.trailer_key_handle(b"QTest");
    pdf.resolve(&original)?;
    let chased = original.clone();
    let terminal_ref = original.object_ref();
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    // qpdf's own `hasKey` treats a key resolving to null the same as a
    // missing key (`libqpdf/QPDF_Dictionary.cc:98-100`) — an *explicit*
    // `/QTest null` entry is "implicit" here too, not just a genuinely
    // absent key, so this checks the chased terminal value rather than
    // `ObjectHandle::has_key` (which only reports raw map presence; see its
    // own doc distinguishing it from this exact case).
    if chased.is_null() {
        writeln!(stdout, "/QTest is implicit")?;
    }

    let direct_prefix = if original.object_ref().is_some() {
        "in"
    } else {
        ""
    };
    let type_name = chased.type_name()?;
    let type_code = chased.type_code()?;
    write!(stdout, "/QTest is {direct_prefix}direct and has type ")?;
    writeln!(stdout, "{type_name} ({type_code})")?;

    let details = write_object_details(
        pdf,
        filename,
        stdout,
        stderr,
        diagnostics_written,
        &chased,
        terminal_ref,
    );
    details?;

    // The canonical handle serializer performs qpdf's eager dictionary
    // null-visibility walk while retaining indirect child identity. The
    // `resolve` has already resolved the canonical root handle in place, so
    // this helper reuses the same live object rather than materializing a
    // second value model.
    let (resolved, _, _) = resolve_handle(pdf, &original)?;
    let unparse_bytes = if original.is_indirect() {
        original.unparse()
    } else {
        write_qpdf_object_handle(pdf, &original)?
    };
    let unparse_resolved_bytes = if resolved.as_stream_dict().is_some() {
        unparse_bytes.clone()
    } else {
        write_qpdf_object_handle(pdf, &resolved)?
    };

    write!(stdout, "unparse: ")?;
    write_bytes(stdout, &unparse_bytes)?;
    writeln!(stdout)?;
    write!(stdout, "unparseResolved: ")?;
    write_bytes(stdout, &unparse_resolved_bytes)?;
    writeln!(stdout)?;
    Ok(())
}

// `dict`'s entries are resolved once through the canonical object cache to
// decide null-omission, mirroring qpdf's own `hasKey`/`getKeys`/`ditems()` rule
// that a dictionary entry resolving to null is equivalent to a missing key
// (`libqpdf/QPDF_Dictionary.cc:98-125`). Indirectness in the returned pairs
// reflects the child handle's own identity after that one cache resolution.
fn dictionary_items<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
) -> flpdf::Result<Vec<(Vec<u8>, bool)>> {
    let entries = dict
        .as_dictionary()
        .ok_or_else(|| Error::System("dictionary access on non-dictionary object".to_string()))?;
    let mut items = Vec::new();
    for (key, child) in entries {
        pdf.resolve(&child)?;
        if !child.is_null() {
            items.push((key, child.object_ref().is_some()));
        }
    }
    Ok(items)
}

fn pipe_stream_data_with_ordered_diagnostics<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    stream: &ObjectHandle,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<bool> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(OrderedStreamWarning {
        events: Arc::clone(&events),
    })));

    let previous_logger = pdf.logger();
    let previous_suppression = pdf.suppress_warnings();
    pdf.set_logger(logger);
    pdf.set_suppress_warnings(false);

    let result: flpdf::Result<bool> = (|| -> flpdf::Result<bool> {
        let filterable = stream.stream_data_filterable(DecodeLevel::All)?;
        if filterable {
            let mut output = OrderedStreamOutput {
                events: Arc::clone(&events),
            };
            let mut filtering_attempted = false;
            let _ = stream.pipe_stream_data(
                &mut output,
                &mut filtering_attempted,
                0,
                DecodeLevel::All,
                false,
                false,
            )?;
        }
        Ok(filterable)
    })();

    pdf.set_logger(previous_logger);
    pdf.set_suppress_warnings(previous_suppression);
    replay_stream_events(&events, stdout, stderr)?;

    let filterable = result?;
    *diagnostics_written = pdf.repair_diagnostics().entries().len();
    Ok(filterable)
}

#[allow(clippy::too_many_arguments)]
fn write_object_details<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
    chased: &ObjectHandle,
    _terminal_ref: Option<ObjectRef>,
) -> flpdf::Result<()> {
    match chased.type_code()? {
        2 => writeln!(stdout, "/QTest is null")?,
        3 => {
            let value = chased
                .as_boolean()
                .expect("type_code confirmed a boolean value");
            let value = if value { "true" } else { "false" };
            writeln!(stdout, "/QTest is Boolean with value {value}")?;
        }
        4 => {
            let value = chased
                .as_integer()
                .expect("type_code confirmed an integer value");
            writeln!(stdout, "/QTest is an integer with value {value}")?;
        }
        5 => {
            write!(stdout, "/QTest is a real number with value ")?;
            write_bytes(stdout, &chased.unparse_resolved())?;
            writeln!(stdout)?;
        }
        7 => {
            let value = chased.as_name().expect("type_code confirmed a name value");
            write!(stdout, "/QTest is a name with value /")?;
            write_bytes(stdout, &value)?;
            writeln!(stdout)?;
        }
        6 => {
            let value = chased
                .as_string()
                .expect("type_code confirmed a string value");
            write!(stdout, "/QTest is a string with value ")?;
            write_bytes(stdout, &value)?;
            writeln!(stdout)?;
        }
        8 => {
            let items = chased
                .as_array()
                .expect("type_code confirmed an array value");
            writeln!(stdout, "/QTest is an array with {} items", items.len())?;
            for (index, item) in items.iter().enumerate() {
                let direct_prefix = if item.object_ref().is_some() {
                    "in"
                } else {
                    ""
                };
                writeln!(stdout, "  item {index} is {direct_prefix}direct")?;
            }
        }
        9 => {
            writeln!(stdout, "/QTest is a dictionary")?;
            let qpdf_flush_result_7 = dictionary_items(pdf, chased);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            let items = qpdf_flush_result_7?;
            for (key, is_indirect) in items {
                write!(stdout, "  /")?;
                write_bytes(stdout, key.strip_prefix(b"/").unwrap_or(&key))?;
                let direct_prefix = if is_indirect { "in" } else { "" };
                writeln!(stdout, " is {direct_prefix}direct")?;
            }
        }
        10 => {
            let dict_handle = chased
                .as_stream_dict()
                .expect("type_code confirmed a stream value");
            let data = chased.get_raw_stream_data()?;
            write!(stdout, "/QTest is a stream.  Dictionary: ")?;
            let qpdf_flush_result_8 = write_qpdf_object_handle(pdf, &dict_handle);
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            let dictionary = qpdf_flush_result_8?;
            write_bytes(stdout, &dictionary)?;
            writeln!(stdout)?;

            writeln!(stdout, "Raw stream data:")?;
            stdout.flush()?;
            write_bytes(stdout, &data)?;
            writeln!(stdout)?;
            writeln!(stdout, "Uncompressed stream data:")?;

            let filterable = pipe_stream_data_with_ordered_diagnostics(
                pdf,
                chased,
                stdout,
                stderr,
                diagnostics_written,
            )?;
            if !filterable {
                writeln!(stdout, "Stream data is not filterable.")?;
                return Ok(());
            }
            writeln!(stdout)?;
            writeln!(stdout, "End of stream data")?;
        }
        // 11 = operator, 12 = inline-image, 13 = unresolved. Parsed PDF
        // objects cannot carry a bare reference as their whole object value.
        _ => {
            writeln!(stdout, "/QTest is an unknown object")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use super::{run_test_0_1, write_object_details};
    use flpdf::{ObjectHandle, ObjectRef, Pdf, PdfOpenOptions, Pipeline};
    use std::io::{self, Write};

    struct WriteFailure;

    impl Write for WriteFailure {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_failure_fixture_fails_writes_but_allows_flush() {
        let mut writer = WriteFailure;

        assert_eq!(
            writer.write(b"warning").unwrap_err().to_string(),
            "write failed"
        );
        writer.flush().expect("flush remains independently usable");
    }

    #[test]
    fn ordered_stream_sinks_replay_data_and_warnings_in_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut output = super::OrderedStreamOutput {
            events: Arc::clone(&events),
        };
        assert_eq!(output.identifier(), "qtest stream output");
        output.write(b"decoded").expect("record stream data");
        output.finish().expect("finish stream output");

        let mut warning = super::OrderedStreamWarning {
            events: Arc::clone(&events),
        };
        assert_eq!(warning.identifier(), "qtest stream warnings");
        warning.write(b"warning\n").expect("record stream warning");
        warning.finish().expect("finish stream warning");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        super::replay_stream_events(&events, &mut stdout, &mut stderr)
            .expect("replay ordered stream events");
        assert_eq!(stdout, b"decoded");
        assert_eq!(stderr, b"warning\n");
    }

    fn pdf_with_qtest(qtest: &[u8], extras: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let max_object = extras.iter().map(|(number, _)| *number).max().unwrap_or(2);
        let mut objects = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Count 0 /Kids [ ] >>".to_vec()),
        ];
        objects.extend(extras.iter().cloned());
        objects.sort_by_key(|(number, _)| *number);

        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![None; (max_object + 1) as usize];
        for (number, body) in objects {
            offsets[number as usize] = Some(bytes.len());
            bytes.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            bytes.extend_from_slice(&body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", max_object + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            match offset {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 00000 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R", max_object + 1).as_bytes(),
        );
        if !qtest.is_empty() {
            bytes.extend_from_slice(b" /QTest ");
            bytes.extend_from_slice(qtest);
        }
        bytes.extend_from_slice(format!(" >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    fn output_channels(qtest: &[u8], extras: &[(u32, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let bytes = pdf_with_qtest(qtest, extras);
        let options = PdfOpenOptions {
            repair: true,
            description: b"fixture.pdf".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open test_0_1 fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();
        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");
        (stdout, stderr)
    }

    fn output(qtest: &[u8], extras: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let (stdout, stderr) = output_channels(qtest, extras);
        assert!(stderr.is_empty());
        stdout
    }

    #[test]
    fn wrong_object_stream_type_warning_preserves_qpdf_object_context() {
        let mut bytes =
            include_bytes!("../../../../tests/fixtures/compat/three-page-objstm.pdf").to_vec();
        let marker = b"/ObjStm";
        let marker_start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("object-stream type marker");
        bytes[marker_start..marker_start + marker.len()].copy_from_slice(b"/Potato");

        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                repair: true,
                description: b"fixture.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open wrong-object-stream fixture");
        let qtest = pdf.get_object_handle(ObjectRef::new(2, 0));
        pdf.trailer()
            .replace_key(b"/QTest", qtest)
            .expect("attach compressed object to /QTest");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test 0");

        assert_eq!(
            stderr,
            b"WARNING: fixture.pdf (object 1 0, offset 532): supposed object stream 1 has wrong type\n"
        );
    }

    #[test]
    fn a_parseable_deeply_nested_sibling_trailer_entry_does_not_erase_qtest() {
        // Regression: `run_test_0_1` must read `/QTest` via
        // `Pdf::trailer_key_handle`, not `Pdf::trailer().get_key(...)`
        // — the latter lifts the *entire* trailer in one structural walk
        // bounded by the parser's acceptance limit, so an unrelated sibling
        // entry nested past that bound degrades the whole trailer handle to
        // null, silently reporting `/QTest` as implicit/null while the
        // (unaffected) legacy `unparse`/`unparseResolved` lines still printed
        // its real value — internally contradictory output.
        let mut bytes = pdf_with_qtest(b"true", &[]);
        let marker = b" >>\nstartxref";
        let marker_start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("trailer close token");
        let mut deep = b"1".to_vec();
        for _ in 0..300 {
            deep = [b"[ ".as_slice(), &deep, b" ]".as_slice()].concat();
        }
        let mut sibling = b" /Deep ".to_vec();
        sibling.extend_from_slice(&deep);
        bytes.splice(marker_start..marker_start, sibling);

        let options = PdfOpenOptions {
            repair: true,
            description: b"fixture.pdf".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open sibling-nesting fixture");
        assert!(
            pdf.trailer().as_dictionary().is_some(),
            "a parseable 300-level trailer must remain available as a handle"
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();
        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");

        assert_eq!(
            stdout,
            b"/QTest is direct and has type boolean (3)\n\
              /QTest is Boolean with value true\n\
              unparse: true\n\
              unparseResolved: true\n"
        );
    }

    #[test]
    fn a_directly_nested_qtest_value_past_max_inline_depth_is_not_erased() {
        // Deep nesting in an *unrelated* sibling must not erase `/QTest` —
        // `/QTest` itself, nested between the crate's inline-object-nesting
        // limit (256) and the parser's own, higher, acceptance limit (500),
        // parses successfully and must be reported as the array it is, not
        // silently degraded to implicit/null by `trailer_key_handle`'s own
        // lift bound being tighter than what the canonical handle resolver
        // already accepts for the same value.
        let mut deep = b"1".to_vec();
        for _ in 0..300 {
            deep = [b"[ ".as_slice(), &deep, b" ]".as_slice()].concat();
        }

        let stdout = output(&deep, &[]);

        assert!(
            stdout.starts_with(b"/QTest is direct and has type array (8)\n"),
            "stdout: {}",
            String::from_utf8_lossy(&stdout)
        );
        assert!(
            !stdout
                .windows(b"/QTest is implicit\n".len())
                .any(|window| window == b"/QTest is implicit\n"),
            "a successfully parsed deep value must not be reported as implicit"
        );
    }

    #[test]
    fn missing_qtest_has_exact_implicit_null_output() {
        assert_eq!(
            output(b"", &[]),
            b"/QTest is implicit\n\
              /QTest is direct and has type null (2)\n\
              /QTest is null\n\
              unparse: null\n\
              unparseResolved: null\n"
        );
    }

    #[test]
    fn booleans_emit_their_actual_value() {
        for (qtest, value) in [(b"true".as_slice(), "true"), (b"false".as_slice(), "false")] {
            assert_eq!(
                output(qtest, &[]),
                format!(
                    "/QTest is direct and has type boolean (3)\n\
                     /QTest is Boolean with value {value}\n\
                     unparse: {value}\n\
                     unparseResolved: {value}\n"
                )
                .into_bytes()
            );
        }
    }

    #[test]
    fn integer_real_name_and_string_outputs_match_qpdf_literals() {
        let cases: &[(&[u8], &[u8])] = &[
            (
                b"42",
                b"/QTest is direct and has type integer (4)\n\
                  /QTest is an integer with value 42\n\
                  unparse: 42\n\
                  unparseResolved: 42\n",
            ),
            (
                b"1.50",
                b"/QTest is direct and has type real (5)\n\
                  /QTest is a real number with value 1.50\n\
                  unparse: 1.50\n\
                  unparseResolved: 1.50\n",
            ),
            (
                b"/A#20B",
                b"/QTest is direct and has type name (7)\n\
                  /QTest is a name with value /A B\n\
                  unparse: /A#20B\n\
                  unparseResolved: /A#20B\n",
            ),
            (
                b"(hello)",
                b"/QTest is direct and has type string (6)\n\
                  /QTest is a string with value hello\n\
                  unparse: (hello)\n\
                  unparseResolved: (hello)\n",
            ),
        ];
        for (qtest, expected) in cases {
            assert_eq!(output(qtest, &[]), *expected);
        }
    }

    #[test]
    fn array_reports_each_items_indirectness_without_resolving_unparse_children() {
        assert_eq!(
            output(b"[ 1 7 0 R 0.0 ]", &[(7, b"true".to_vec())]),
            concat!(
                "/QTest is direct and has type array (8)\n",
                "/QTest is an array with 3 items\n",
                "  item 0 is direct\n",
                "  item 1 is indirect\n",
                "  item 2 is direct\n",
                "unparse: [ 1 7 0 R 0.0 ]\n",
                "unparseResolved: [ 1 7 0 R 0.0 ]\n",
            )
            .as_bytes()
        );
    }

    #[test]
    fn array_reports_indirect_child_without_resolving_its_target() {
        let bytes = pdf_with_qtest(b"[ 100 0 R ]", &[]);
        let options = PdfOpenOptions {
            repair: true,
            description: b"fixture.pdf".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open test_0_1 fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();
        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");

        assert!(stdout
            .windows(b"  item 0 is indirect\n".len())
            .any(|line| line == b"  item 0 is indirect\n"));
    }

    #[test]
    fn dictionary_omits_resolved_nulls_and_uses_decoded_display_names() {
        assert_eq!(
            output(
                b"<< /b false /a 7 0 R /gone 99 0 R /hex#20strings true >>",
                &[(7, b"true".to_vec())],
            ),
            concat!(
                "/QTest is direct and has type dictionary (9)\n",
                "/QTest is a dictionary\n",
                "  /a is indirect\n",
                "  /b is direct\n",
                "  /hex strings is direct\n",
                "unparse: << /a 7 0 R /b false /hex#20strings true >>\n",
                "unparseResolved: << /a 7 0 R /b false /hex#20strings true >>\n",
            )
            .as_bytes()
        );
    }

    #[test]
    fn dictionary_lazy_child_warning_is_drained_before_items_even_when_null() {
        let bytes = pdf_with_qtest(
            b"6 0 R",
            &[
                (6, b"<< /a 7 0 R >>".to_vec()),
                (7, b"null\nnot-endobj".to_vec()),
            ],
        );
        let warning_offset = bytes
            .windows(b"not-endobj".len())
            .position(|window| window == b"not-endobj")
            .expect("malformed child token");
        let options = PdfOpenOptions {
            repair: true,
            description: b"fixture.pdf".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open lazy child fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");

        assert_eq!(
            stdout,
            b"/QTest is indirect and has type dictionary (9)\n\
              /QTest is a dictionary\n\
              unparse: 6 0 R\n\
              unparseResolved: << >>\n"
        );
        assert_eq!(
            stderr,
            format!(
                "WARNING: fixture.pdf (object 7 0, offset {warning_offset}): expected endobj\n"
            )
            .into_bytes()
        );
    }

    #[test]
    fn flate_stream_emits_raw_and_decoded_bytes_and_indirect_unparse() {
        let compressed = b"\x78\x9c\x4b\x4c\x4a\x06\x00\x02\x4d\x01\x27";
        let mut stream = b"<< /Filter /FlateDecode /Length 11 >>\nstream\n".to_vec();
        stream.extend_from_slice(compressed);
        stream.extend_from_slice(b"\nendstream");

        let mut expected = b"/QTest is indirect and has type stream (10)\n\
                             /QTest is a stream.  Dictionary: << /Filter /FlateDecode /Length 11 >>\n\
                             Raw stream data:\n"
            .to_vec();
        expected.extend_from_slice(compressed);
        expected.extend_from_slice(
            b"\nUncompressed stream data:\n\
              abc\n\
              End of stream data\n\
              unparse: 7 0 R\n\
              unparseResolved: 7 0 R\n",
        );
        assert_eq!(output(b"7 0 R", &[(7, stream)]), expected);
    }

    #[test]
    fn crypt_stream_is_driver_only_identity_after_qpdf_decryption_boundary() {
        assert_eq!(
            output(
                b"7 0 R",
                &[(
                    7,
                    b"<< /Filter /Crypt /Length 3 >>\nstream\nabc\nendstream".to_vec(),
                )],
            ),
            b"/QTest is indirect and has type stream (10)\n\
              /QTest is a stream.  Dictionary: << /Filter /Crypt /Length 3 >>\n\
              Raw stream data:\n\
              abc\n\
              Uncompressed stream data:\n\
              abc\n\
              End of stream data\n\
              unparse: 7 0 R\n\
              unparseResolved: 7 0 R\n"
        );
    }

    #[test]
    fn crypt_decode_params_follow_qpdf_validation() {
        let compressed = b"\x78\x9c\x4b\x4c\x4a\x06\x00\x02\x4d\x01\x27";
        let mut stream = b"<< /Filter [ /Crypt /FlateDecode ] \
                           /DecodeParms [ << /Type /CryptFilterDecodeParms /Name 42 >> null ] \
                           /Length 11 >>\nstream\n"
            .to_vec();
        stream.extend_from_slice(compressed);
        stream.extend_from_slice(b"\nendstream");

        let actual = output(b"7 0 R", &[(7, stream)]);

        assert!(actual
            .windows(b"\nUncompressed stream data:\nabc\nEnd of stream data\n".len())
            .any(|line| line == b"\nUncompressed stream data:\nabc\nEnd of stream data\n"));
    }

    #[test]
    fn flate_ignores_deep_unknown_decode_parameter_values() {
        let metadata = (0..64).fold(b"1".to_vec(), |value, _| {
            let mut nested = b"[ ".to_vec();
            nested.extend_from_slice(&value);
            nested.extend_from_slice(b" ]");
            nested
        });
        let compressed = b"\x78\x9c\x4b\x4c\x4a\x06\x00\x02\x4d\x01\x27";
        let mut stream = b"<< /Filter /FlateDecode /DecodeParms << /Metadata ".to_vec();
        stream.extend_from_slice(&metadata);
        stream.extend_from_slice(b" >> /Length 11 >>\nstream\n");
        stream.extend_from_slice(compressed);
        stream.extend_from_slice(b"\nendstream");

        let actual = output(b"7 0 R", &[(7, stream)]);
        assert!(actual
            .windows(b"\nabc\nEnd of stream data\n".len())
            .any(|line| { line == b"\nabc\nEnd of stream data\n" }));
    }

    #[test]
    fn nondictionary_flate_decode_params_warn_twice_at_the_value_token() {
        let compressed = b"\x78\x9c\x4b\x4c\x4a\x06\x00\x02\x4d\x01\x27";
        let mut stream =
            b"<< /Filter /FlateDecode /DecodeParms 42 /Length 11 >>\nstream\n".to_vec();
        stream.extend_from_slice(compressed);
        stream.extend_from_slice(b"\nendstream");
        let bytes = pdf_with_qtest(b"7 0 R", &[(7, stream)]);
        let marker = b"/DecodeParms 42";
        let marker_start = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("DecodeParms source token");
        let value_offset = marker_start + b"/DecodeParms ".len();
        let options = PdfOpenOptions {
            repair: true,
            description: b"fixture.pdf".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open DecodeParms fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");

        assert!(stdout
            .windows(b"\nUncompressed stream data:\nabc\nEnd of stream data\n".len())
            .any(|line| line == b"\nUncompressed stream data:\nabc\nEnd of stream data\n"));
        let warning = format!(
            "WARNING: fixture.pdf, object 7 0 at offset {value_offset}: \
             operation for dictionary attempted on object of type integer: treating as empty\n"
        );
        assert_eq!(stderr, warning.repeat(2).into_bytes());
    }

    #[test]
    fn indirect_decode_params_array_warning_offsets_are_batched() {
        let stream = b"<< /Filter [ /FlateDecode /LZWDecode ] /DecodeParms 8 0 R /Length 0 >>\n\
                       stream\n\nendstream"
            .to_vec();
        let array = b"[ 42 43 ]".to_vec();
        let bytes = pdf_with_qtest(b"7 0 R", &[(7, stream), (8, array)]);
        let first = bytes
            .windows(b"[ 42 43 ]".len())
            .position(|window| window == b"[ 42 43 ]")
            .expect("DecodeParms array")
            + b"[ ".len();
        let second = first + b"42 ".len();
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                description: b"fixture.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open indirect DecodeParms array");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");

        let first_warning = format!(
            "WARNING: fixture.pdf, object 8 0 at offset {first}: operation for dictionary attempted \
             on object of type integer: treating as empty\n"
        );
        let second_warning = format!(
            "WARNING: fixture.pdf, object 8 0 at offset {second}: operation for dictionary attempted \
             on object of type integer: treating as empty\n"
        );
        assert_eq!(
            stderr,
            [
                first_warning.as_bytes(),
                second_warning.as_bytes(),
                first_warning.as_bytes(),
                second_warning.as_bytes(),
            ]
            .concat()
        );
    }

    #[test]
    fn indirect_decode_params_scalar_warning_offsets_are_batched() {
        let stream = b"<< /Filter [ /FlateDecode /LZWDecode ] /DecodeParms 8 0 R /Length 0 >>\n\
                       stream\n\nendstream"
            .to_vec();
        let scalar = b"42".to_vec();
        let bytes = pdf_with_qtest(b"7 0 R", &[(7, stream), (8, scalar)]);
        let offset = bytes
            .windows(b"8 0 obj".len())
            .position(|window| window == b"8 0 obj")
            .expect("DecodeParms scalar")
            + b"8 0 obj".len();
        let mut pdf = Pdf::open_mem_owned_with_options(
            bytes,
            PdfOpenOptions {
                description: b"fixture.pdf".to_vec(),
                ..PdfOpenOptions::default()
            },
        )
        .expect("open indirect DecodeParms scalar");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        run_test_0_1(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");

        let warning = format!(
            "WARNING: fixture.pdf, object 8 0 at offset {offset}: operation for dictionary attempted \
             on object of type integer: treating as empty\n"
        );
        assert_eq!(stderr, warning.repeat(4).into_bytes());
    }

    #[test]
    fn corrupt_flate_is_filterable_and_finishes_after_warning() {
        let (stdout, stderr) = output_channels(
            b"7 0 R",
            &[(
                7,
                b"<< /Filter /FlateDecode /Length 3 >>\nstream\nabc\nendstream".to_vec(),
            )],
        );

        assert_eq!(
            stdout,
            b"/QTest is indirect and has type stream (10)\n\
              /QTest is a stream.  Dictionary: << /Filter /FlateDecode /Length 3 >>\n\
              Raw stream data:\n\
              abc\n\
              Uncompressed stream data:\n\
              \n\
              End of stream data\n\
              unparse: 7 0 R\n\
              unparseResolved: 7 0 R\n"
        );
        assert_eq!(
            stderr,
            b"WARNING: fixture.pdf (offset 163): error decoding stream data for object 7 0: \
              stream inflate: inflate: data: incorrect header check\n"
        );
    }

    #[test]
    fn chained_filter_emits_write_error_before_finish_warning() {
        let (stdout, stderr) = output_channels(
            b"7 0 R",
            &[(
                7,
                b"<< /Filter [ /ASCIIHexDecode /FlateDecode ] /Length 3 >>\n\
                  stream\n78G\nendstream"
                    .to_vec(),
            )],
        );

        assert!(stdout
            .windows(b"\nUncompressed stream data:\n\nEnd of stream data\n".len())
            .any(|line| line == b"\nUncompressed stream data:\n\nEnd of stream data\n"));
        assert_eq!(
            stderr,
            b"WARNING: fixture.pdf (offset 183): error decoding stream data for object 7 0: \
              character out of range during base Hex decode: G\n\
              WARNING: fixture.pdf (offset 183): input stream is complete but output may still be valid\n"
        );
    }

    #[test]
    fn nonfatal_flate_warning_is_emitted_before_end_of_stream() {
        let (stdout, stderr) = output_channels(
            b"7 0 R",
            &[(
                7,
                b"<< /Filter /FlateDecode /Length 1 >>\nstream\n\x78\nendstream".to_vec(),
            )],
        );

        assert!(stdout
            .windows(b"\nUncompressed stream data:\n\nEnd of stream data\n".len())
            .any(|line| line == b"\nUncompressed stream data:\n\nEnd of stream data\n"));
        assert_eq!(
            stderr,
            b"WARNING: fixture.pdf (offset 163): input stream is complete but output may still be valid\n"
        );
    }

    #[test]
    fn direct_stream_runtime_error_uses_the_canonical_pipeline_error() {
        let bytes = pdf_with_qtest(b"null", &[]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open direct stream fixture");
        let dict = ObjectHandle::dictionary(vec![(
            b"Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let qtest = ObjectHandle::stream(dict, Rc::new(b"abc".to_vec()));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
            None,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "stream inflate: inflate: data: incorrect header check"
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn direct_stream_decode_param_warning_uses_the_canonical_type_warning() {
        let bytes = pdf_with_qtest(b"null", &[]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open direct stream fixture");
        let dict = ObjectHandle::dictionary(vec![
            (
                b"Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"DecodeParms".to_vec(), ObjectHandle::integer(42)),
        ]);
        let qtest = ObjectHandle::stream(dict, Rc::new(Vec::new()));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
            None,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "operation for dictionary attempted on object of type integer: treating as empty"
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn nonfatal_warning_writer_error_is_propagated() {
        let bytes = pdf_with_qtest(
            b"7 0 R",
            &[(
                7,
                b"<< /Filter /FlateDecode /Length 1 >>\nstream\n\x78\nendstream".to_vec(),
            )],
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open warning fixture");
        let original = pdf.trailer_key_handle(b"QTest");
        pdf.resolve(&original).expect("resolve qtest");
        let qtest = original.clone();
        let terminal_ref = original.object_ref();
        let mut stdout = Vec::new();
        let mut stderr = WriteFailure;
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
            terminal_ref,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "I/O error: write failed");
        assert!(!stdout
            .windows(b"End of stream data\n".len())
            .any(|line| line == b"End of stream data\n"));
    }

    #[test]
    fn runtime_error_warning_writer_error_is_propagated() {
        let bytes = pdf_with_qtest(
            b"7 0 R",
            &[(
                7,
                b"<< /Filter /FlateDecode /Length 3 >>\nstream\nabc\nendstream".to_vec(),
            )],
        );
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open codec-error fixture");
        let original = pdf.trailer_key_handle(b"QTest");
        pdf.resolve(&original).expect("resolve qtest");
        let qtest = original.clone();
        let terminal_ref = original.object_ref();
        let mut stdout = Vec::new();
        let mut stderr = WriteFailure;
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
            terminal_ref,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "I/O error: write failed");
        assert!(!stdout
            .windows(b"End of stream data\n".len())
            .any(|line| line == b"End of stream data\n"));
    }

    #[test]
    fn unsupported_stream_filter_reports_not_filterable() {
        assert_eq!(
            output(
                b"7 0 R",
                &[(
                    7,
                    b"<< /Filter /BogusDecode /Length 3 >>\nstream\nabc\nendstream".to_vec(),
                )],
            ),
            b"/QTest is indirect and has type stream (10)\n\
              /QTest is a stream.  Dictionary: << /Filter /BogusDecode /Length 3 >>\n\
              Raw stream data:\n\
              abc\n\
              Uncompressed stream data:\n\
              Stream data is not filterable.\n\
              unparse: 7 0 R\n\
              unparseResolved: 7 0 R\n"
        );
    }

    #[test]
    fn unsupported_decode_parameters_report_not_filterable() {
        assert_eq!(
            output(
                b"7 0 R",
                &[(
                    7,
                    b"<< /Filter /ASCIIHexDecode /DecodeParms << /Foo 1 >> /Length 3 >>\n\
                      stream\n41>\nendstream"
                        .to_vec(),
                )],
            ),
            b"/QTest is indirect and has type stream (10)\n\
              /QTest is a stream.  Dictionary: << /DecodeParms << /Foo 1 >> /Filter /ASCIIHexDecode /Length 3 >>\n\
              Raw stream data:\n\
              41>\n\
              Uncompressed stream data:\n\
              Stream data is not filterable.\n\
              unparse: 7 0 R\n\
              unparseResolved: 7 0 R\n"
        );
    }

    #[test]
    fn decode_parms_length_mismatch_reports_qpdf_warning() {
        let (stdout, stderr) = output_channels(
            b"7 0 R",
            &[(
                7,
                b"<< /Filter [ /FlateDecode /FlateDecode ] /DecodeParms [ null ] /Length 3 >>\n\
                  stream\nabc\nendstream"
                    .to_vec(),
            )],
        );
        assert_eq!(
            stdout,
            b"/QTest is indirect and has type stream (10)\n\
              /QTest is a stream.  Dictionary: << /DecodeParms [ null ] /Filter [ /FlateDecode /FlateDecode ] /Length 3 >>\n\
              Raw stream data:\n\
              abc\n\
              Uncompressed stream data:\n\
              Stream data is not filterable.\n\
              unparse: 7 0 R\n\
              unparseResolved: 7 0 R\n"
        );
        assert_eq!(
            stderr,
            b"WARNING: fixture.pdf (offset 202): stream /DecodeParms length is inconsistent with filters\n"
        );
    }

    #[test]
    fn content_only_object_uses_the_unknown_object_branch() {
        let bytes = pdf_with_qtest(b"null", &[]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open operator test PDF");
        let qtest = ObjectHandle::operator(b"q".to_vec());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        write_object_details(
            &mut pdf,
            b"fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
            None,
        )
        .expect("write operator details");
        assert_eq!(stdout, b"/QTest is an unknown object\n");
        assert!(stderr.is_empty());
    }
}
