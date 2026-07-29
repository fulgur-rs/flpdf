use std::io::{Read, Seek, Write};

use flpdf::filters::StreamDecodeEvent;
use flpdf::{Diagnostic, Error, Object, Pdf};

use super::handle::{resolve_stream_dictionary, write_qpdf_object, Handle};
use super::{emit_new_diagnostics, write_warning};
use crate::output::write_bytes;

fn stream_decode_error_detail(error: Error) -> String {
    match error {
        Error::Unsupported(message) | Error::Internal(message) | Error::System(message) => message,
        error => error.to_string(),
    }
}

pub(crate) fn run_test_0_1<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let trailer = pdf.trailer().clone();
    let qtest = Handle::get_key(pdf, &trailer, b"QTest")?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;

    if !Handle::has_key(pdf, &trailer, b"QTest")? {
        writeln!(stdout, "/QTest is implicit")?;
    }

    let direct_prefix = if qtest.is_indirect() { "in" } else { "" };
    let type_name = qtest.type_name();
    let type_code = qtest.type_code();
    write!(stdout, "/QTest is {direct_prefix}direct and has type ")?;
    writeln!(stdout, "{type_name} ({type_code})")?;

    let details = write_object_details(pdf, filename, stdout, stderr, diagnostics_written, &qtest);
    details?;

    write!(stdout, "unparse: ")?;
    write_bytes(stdout, &qtest.unparse(pdf)?)?;
    writeln!(stdout)?;
    write!(stdout, "unparseResolved: ")?;
    write_bytes(stdout, &qtest.unparse_resolved(pdf)?)?;
    writeln!(stdout)?;
    Ok(())
}

fn write_object_details<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
    qtest: &Handle,
) -> flpdf::Result<()> {
    match qtest.resolved() {
        Object::Null => writeln!(stdout, "/QTest is null")?,
        Object::Boolean(_) => {
            let value = qtest
                .as_bool()
                .expect("boolean branch must retain a boolean value");
            let value = if value { "true" } else { "false" };
            writeln!(stdout, "/QTest is Boolean with value {value}")?;
        }
        Object::Integer(value) => {
            writeln!(stdout, "/QTest is an integer with value {value}")?;
        }
        Object::Real(_) | Object::RealLiteral { .. } => {
            write!(stdout, "/QTest is a real number with value ")?;
            write_bytes(stdout, &qtest.unparse_resolved(pdf)?)?;
            writeln!(stdout)?;
        }
        Object::Name(value) => {
            write!(stdout, "/QTest is a name with value /")?;
            write_bytes(stdout, value)?;
            writeln!(stdout)?;
        }
        Object::String(value) => {
            write!(stdout, "/QTest is a string with value ")?;
            write_bytes(stdout, value)?;
            writeln!(stdout)?;
        }
        Object::Array(values) => {
            writeln!(stdout, "/QTest is an array with {} items", values.len())?;
            for (index, is_indirect) in qtest.array_item_indirectness()?.into_iter().enumerate() {
                let direct_prefix = if is_indirect { "in" } else { "" };
                writeln!(stdout, "  item {index} is {direct_prefix}direct")?;
            }
        }
        Object::Dictionary(_) => {
            writeln!(stdout, "/QTest is a dictionary")?;
            for (key, value) in qtest.dictionary_items(pdf)? {
                write!(stdout, "  /")?;
                write_bytes(stdout, &key)?;
                let direct_prefix = if value.is_indirect() { "in" } else { "" };
                writeln!(stdout, " is {direct_prefix}direct")?;
            }
        }
        Object::Stream(stream) => {
            let stream = stream.clone();
            write!(stdout, "/QTest is a stream.  Dictionary: ")?;
            let dictionary = write_qpdf_object(pdf, &Object::Dictionary(stream.dict.clone()))?;
            let decode_dictionary = resolve_stream_dictionary(pdf, &stream.dict)?;
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            write_bytes(stdout, &dictionary)?;
            writeln!(stdout)?;

            writeln!(stdout, "Raw stream data:")?;
            stdout.flush()?;
            write_bytes(stdout, &stream.data)?;
            writeln!(stdout)?;
            writeln!(stdout, "Uncompressed stream data:")?;

            if !decode_dictionary.is_filterable() {
                writeln!(stdout, "Stream data is not filterable.")?;
                return Ok(());
            }

            match flpdf::filters::decode_stream_data_recovering(&decode_dictionary, &stream.data) {
                Ok(decoded) => {
                    let terminal_ref = qtest.terminal_indirect_ref();
                    let offset = terminal_ref
                        .map(|object_ref| pdf.source_stream_data_offset(object_ref))
                        .transpose()?
                        .flatten();
                    stdout.flush()?;
                    for event in decoded.events {
                        match event {
                            StreamDecodeEvent::Data(data) => write_bytes(stdout, &data)?,
                            StreamDecodeEvent::Warning(warning) => {
                                write_warning(
                                    filename,
                                    &Diagnostic::warning(warning.message, offset),
                                    stdout,
                                    stderr,
                                )?;
                            }
                            StreamDecodeEvent::Error(error) => {
                                let object_ref = terminal_ref.ok_or_else(|| {
                                    Error::System(
                                        "decoded stream has no terminal indirect object"
                                            .to_string(),
                                    )
                                })?;
                                let detail = stream_decode_error_detail(error);
                                write_warning(
                                    filename,
                                    &Diagnostic::warning(
                                        format!(
                                            "error decoding stream data for object {} {}: {detail}",
                                            object_ref.number, object_ref.generation
                                        ),
                                        offset,
                                    ),
                                    stdout,
                                    stderr,
                                )?;
                            }
                        }
                    }
                    writeln!(stdout)?;
                    writeln!(stdout, "End of stream data")?;
                }
                Err(Error::Unsupported(message))
                    if message == "stream filter type is not name or array"
                        || message == "stream /DecodeParms length is inconsistent with filters" =>
                {
                    let offset = qtest
                        .terminal_indirect_ref()
                        .map(|object_ref| pdf.source_stream_data_offset(object_ref))
                        .transpose()?
                        .flatten();
                    let diagnostic = Diagnostic::warning(message, offset);
                    write_warning(filename, &diagnostic, stdout, stderr)?;
                    writeln!(stdout, "Stream data is not filterable.")?;
                }
                Err(_) => writeln!(stdout, "Stream data is not filterable.")?,
            }
        }
        Object::Operator(_) | Object::InlineImage(_) | Object::Reference(_) => {
            writeln!(stdout, "/QTest is an unknown object")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_test_0_1, stream_decode_error_detail, write_object_details};
    use crate::driver::handle::Handle;
    use flpdf::{Dictionary, Error, Object, ObjectRef, Pdf, PdfOpenOptions, Stream};
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
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open test_0_1 fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();
        run_test_0_1(
            &mut pdf,
            "fixture.pdf",
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
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open test_0_1 fixture");
        for number in 100..=164 {
            let value = if number == 164 {
                Object::Boolean(true)
            } else {
                Object::Reference(ObjectRef::new(number + 1, 0))
            };
            pdf.set_object(ObjectRef::new(number, 0), value);
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();
        run_test_0_1(
            &mut pdf,
            "fixture.pdf",
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
    fn direct_stream_runtime_error_requires_a_terminal_object_reference() {
        let bytes = pdf_with_qtest(b"null", &[]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open direct stream fixture");
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let qtest = Handle::from_value(
            &mut pdf,
            Object::Stream(Stream::new(dictionary, b"abc".to_vec())),
        )
        .expect("construct direct stream handle");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            "fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "decoded stream has no terminal indirect object"
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn stream_decode_error_detail_formats_every_match_arm() {
        let cases = [
            (Error::Unsupported("unsupported".to_string()), "unsupported"),
            (Error::Internal("internal".to_string()), "internal"),
            (Error::System("system".to_string()), "system"),
            (Error::parse(4, "parse"), "parse error at byte 4: parse"),
        ];

        for (error, expected) in cases {
            assert_eq!(stream_decode_error_detail(error), expected);
        }
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
        let trailer = pdf.trailer().clone();
        let qtest = Handle::get_key(&mut pdf, &trailer, b"QTest").expect("get qtest");
        let mut stdout = Vec::new();
        let mut stderr = WriteFailure;
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            "fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
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
        let trailer = pdf.trailer().clone();
        let qtest = Handle::get_key(&mut pdf, &trailer, b"QTest").expect("get qtest");
        let mut stdout = Vec::new();
        let mut stderr = WriteFailure;
        let mut diagnostics_written = 0;

        let error = write_object_details(
            &mut pdf,
            "fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
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
    fn chained_stream_warning_uses_terminal_stream_offset() {
        let stream = b"<< /Filter [ /FlateDecode /FlateDecode ] \
                       /DecodeParms [ null ] /Length 3 >>\n\
                       stream\nabc\nendstream"
            .to_vec();
        let bytes = pdf_with_qtest(b"6 0 R", &[(6, b"7 0 R".to_vec()), (7, stream)]);
        let options = PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        };
        let mut pdf =
            Pdf::open_mem_owned_with_options(bytes, options).expect("open chained stream fixture");
        pdf.set_object(
            flpdf::ObjectRef::new(6, 0),
            Object::Reference(flpdf::ObjectRef::new(7, 0)),
        );
        let terminal_offset = pdf
            .source_stream_data_offset(flpdf::ObjectRef::new(7, 0))
            .expect("locate terminal stream")
            .expect("terminal stream offset");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = pdf.repair_diagnostics().entries().len();

        run_test_0_1(
            &mut pdf,
            "fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run chained stream fixture");

        assert_eq!(
            stderr,
            format!(
                "WARNING: fixture.pdf (offset {terminal_offset}): \
                 stream /DecodeParms length is inconsistent with filters\n"
            )
            .into_bytes()
        );
    }

    #[test]
    fn content_only_object_uses_the_unknown_object_branch() {
        let bytes = pdf_with_qtest(b"null", &[]);
        let mut pdf = Pdf::open_mem_owned(bytes).expect("open operator test PDF");
        let qtest =
            Handle::from_value(&mut pdf, Object::Operator(b"q".to_vec())).expect("operator handle");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        write_object_details(
            &mut pdf,
            "fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
            &qtest,
        )
        .expect("write operator details");
        assert_eq!(stdout, b"/QTest is an unknown object\n");
        assert!(stderr.is_empty());
    }
}
