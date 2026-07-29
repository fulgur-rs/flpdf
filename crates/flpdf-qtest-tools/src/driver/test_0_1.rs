use std::io::{Read, Seek, Write};

use flpdf::{Diagnostic, Error, Object, ObjectRef, Pdf};

use super::handle::{resolve_stream_dictionary, Handle};
use super::{emit_new_diagnostics, write_warning};
use crate::output::write_bytes;

pub(crate) fn run_test_0_1<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source: &[u8],
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

    let details = write_object_details(
        pdf,
        source,
        filename,
        stdout,
        stderr,
        diagnostics_written,
        &qtest,
    );
    details?;

    write!(stdout, "unparse: ")?;
    write_bytes(stdout, &qtest.unparse())?;
    writeln!(stdout)?;
    write!(stdout, "unparseResolved: ")?;
    write_bytes(stdout, &qtest.unparse_resolved())?;
    writeln!(stdout)?;
    Ok(())
}

fn write_object_details<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    source: &[u8],
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
            write_bytes(stdout, &qtest.unparse_resolved())?;
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
            for (index, item) in qtest.array_items(pdf)?.iter().enumerate() {
                let direct_prefix = if item.is_indirect() { "in" } else { "" };
                writeln!(stdout, "  item {index} is {direct_prefix}direct")?;
            }
        }
        Object::Dictionary(_) => {
            writeln!(stdout, "/QTest is a dictionary")?;
            for (key, value) in qtest.dictionary_items(pdf)? {
                write!(stdout, "  ")?;
                let mut name = Vec::new();
                Object::Name(key).write_pdf(&mut name);
                write_bytes(stdout, &name)?;
                let direct_prefix = if value.is_indirect() { "in" } else { "" };
                writeln!(stdout, " is {direct_prefix}direct")?;
            }
        }
        Object::Stream(stream) => {
            let stream = stream.clone();
            write!(stdout, "/QTest is a stream.  Dictionary: ")?;
            let decode_dictionary = resolve_stream_dictionary(pdf, &stream.dict)?;
            emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
            let mut dictionary = Vec::new();
            Object::Dictionary(stream.dict.clone()).write_pdf(&mut dictionary);
            write_bytes(stdout, &dictionary)?;
            writeln!(stdout)?;

            writeln!(stdout, "Raw stream data:")?;
            stdout.flush()?;
            write_bytes(stdout, &stream.data)?;
            writeln!(stdout)?;
            writeln!(stdout, "Uncompressed stream data:")?;

            match flpdf::filters::decode_stream_data(&decode_dictionary, &stream.data) {
                Ok(decoded) => {
                    stdout.flush()?;
                    write_bytes(stdout, &decoded)?;
                    writeln!(stdout)?;
                    writeln!(stdout, "End of stream data")?;
                }
                Err(Error::Unsupported(message))
                    if message == "stream filter type is not name or array" =>
                {
                    let offset = stream_data_offset(source, qtest.indirect_ref());
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

fn stream_data_offset(source: &[u8], object_ref: Option<ObjectRef>) -> Option<u64> {
    let object_ref = object_ref?;
    let header = format!("{} {} obj", object_ref.number, object_ref.generation);
    let line_header = format!("\n{header}");
    let object_start = if source.starts_with(header.as_bytes()) {
        0
    } else {
        find_bytes(source, line_header.as_bytes())? + 1
    };
    let stream_marker = find_bytes(&source[object_start..], b"stream")? + object_start;
    let after_marker = &source[stream_marker + b"stream".len()..];
    let eol_length = if after_marker.starts_with(b"\r\n") {
        2
    } else if after_marker.starts_with(b"\n") {
        1
    } else {
        return None;
    };
    u64::try_from(stream_marker + b"stream".len() + eol_length).ok()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{run_test_0_1, stream_data_offset, write_object_details};
    use crate::driver::handle::Handle;
    use flpdf::{Object, Pdf, PdfOpenOptions};

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

    fn output(qtest: &[u8], extras: &[(u32, Vec<u8>)]) -> Vec<u8> {
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
            &[],
            "fixture.pdf",
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run test_0_1");
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
    fn dictionary_reports_sorted_keys_and_value_indirectness() {
        assert_eq!(
            output(b"<< /b false /a 7 0 R >>", &[(7, b"true".to_vec())],),
            concat!(
                "/QTest is direct and has type dictionary (9)\n",
                "/QTest is a dictionary\n",
                "  /a is indirect\n",
                "  /b is direct\n",
                "unparse: << /a 7 0 R /b false >>\n",
                "unparseResolved: << /a 7 0 R /b false >>\n",
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
    fn stream_data_offsets_accept_lf_and_crlf_framing() {
        let object_ref = Some(flpdf::ObjectRef::new(6, 0));
        assert_eq!(
            stream_data_offset(b"6 0 obj\n<<>>\nstream\nabc", object_ref),
            Some(20)
        );
        assert_eq!(
            stream_data_offset(b"6 0 obj\n<<>>\nstream\r\nabc", object_ref),
            Some(21)
        );
        assert_eq!(stream_data_offset(b"6 0 obj\n<<>>", object_ref), None);
        assert_eq!(
            stream_data_offset(b"16 0 obj\nnull\n6 0 obj\n<<>>\nstream\nabc", object_ref),
            Some(34)
        );
        assert_eq!(
            stream_data_offset(b"6 0 obj\n<<>>\nstreamXabc", object_ref),
            None
        );
        assert_eq!(stream_data_offset(b"", None), None);
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
            &[],
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
