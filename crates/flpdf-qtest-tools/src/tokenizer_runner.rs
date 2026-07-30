use std::ffi::{OsStr, OsString};
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;

use flpdf::filters::{self, DecodeLimits, StreamDecodeEvent};
use flpdf::pages::{page_content_bytes, page_refs};
use flpdf::tokenizer::{token_type_name, TokenType, Tokenizer};
use flpdf::{Object, Pdf, PdfOpenOptions};

use crate::common::test_driver_program_name_bytes;
use crate::driver::{emit_new_diagnostics, os_str_diagnostic_bytes, write_warning};

pub enum RunOutcome {
    Exit(u8),
}

pub fn run(
    args: &[OsString],
    stdout: &mut dyn io::Write,
    stderr: &mut dyn io::Write,
) -> RunOutcome {
    let mut max_len: usize = 0;
    let mut include_ignorable = true;
    let mut filename: Option<OsString> = None;

    let mut i = 1;
    while i < args.len() {
        let arg_str = args[i].to_string_lossy();
        if arg_str.starts_with('-') {
            if arg_str == "-maxlen" {
                i += 1;
                if i >= args.len() {
                    return usage_error(args, stderr);
                }
                max_len = match args[i].to_string_lossy().parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => return usage_error(args, stderr),
                };
            } else if arg_str == "-no-ignorable" {
                include_ignorable = false;
            } else {
                return usage_error(args, stderr);
            }
        } else if filename.is_some() {
            return usage_error(args, stderr);
        } else {
            filename = Some(args[i].clone());
        }
        i += 1;
    }

    let filename = match filename {
        Some(f) => f,
        None => return usage_error(args, stderr),
    };

    match process(&filename, include_ignorable, max_len, stdout, stderr) {
        Ok(()) => RunOutcome::Exit(0),
        Err(e) => {
            let _ = write!(
                stderr,
                "{}: exception: {}",
                String::from_utf8_lossy(&program_name(args)),
                e
            );
            RunOutcome::Exit(2)
        }
    }
}

fn usage_error(args: &[OsString], stderr: &mut dyn io::Write) -> RunOutcome {
    usage(args, stderr);
    RunOutcome::Exit(2)
}

fn usage(args: &[OsString], stderr: &mut dyn io::Write) {
    let name = program_name(args);
    let _ = writeln!(
        stderr,
        "Usage: {} [-maxlen len | -no-ignorable] filename",
        String::from_utf8_lossy(&name)
    );
}

fn program_name(args: &[OsString]) -> Vec<u8> {
    match args.first() {
        Some(argv0) => test_driver_program_name_bytes(&os_str_diagnostic_bytes(argv0)).to_vec(),
        None => b"test_tokenizer".to_vec(),
    }
}

fn sanitize(value: &[u8]) -> String {
    let mut result = String::with_capacity(value.len());
    for &byte in value {
        if (32..=126).contains(&byte) {
            result.push(byte as char);
        } else {
            write!(result, "\\x{byte:02x}").unwrap();
        }
    }
    result
}

fn process(
    filename: &OsStr,
    include_ignorable: bool,
    max_len: usize,
    stdout: &mut dyn io::Write,
    stderr: &mut dyn io::Write,
) -> Result<(), String> {
    let file_bytes = fs::read(Path::new(filename)).map_err(|e| e.to_string())?;

    dump_tokens(
        &file_bytes,
        "FILE",
        max_len,
        include_ignorable,
        true,
        false,
        stdout,
    );

    let options = PdfOpenOptions {
        repair: true,
        allow_weak_crypto: true,
        ..PdfOpenOptions::default()
    };
    let filename_diagnostic = os_str_diagnostic_bytes(filename);
    let mut pdf = match Pdf::open_mem_owned_with_options(file_bytes, options) {
        Ok(pdf) => pdf,
        Err(e) => {
            // qpdf's processFile prints repair warnings as it emits them,
            // even when reconstruction ultimately fails to produce an
            // openable document.
            if let Some((_, diagnostics)) = e.open_failure() {
                for diagnostic in diagnostics.entries() {
                    let _ = write_warning(&filename_diagnostic, diagnostic, stdout, stderr);
                }
            }
            return Err(e.to_string());
        }
    };
    let mut diagnostics_written = 0;
    let _ = emit_new_diagnostics(
        &pdf,
        &mut diagnostics_written,
        &filename_diagnostic,
        stdout,
        stderr,
    );

    let page_refs = page_refs(&mut pdf).map_err(|e| e.to_string())?;
    for (pageno, page_ref) in page_refs.iter().enumerate() {
        let content = page_content_bytes(&mut pdf, *page_ref).unwrap_or_default();
        let label = format!("PAGE {}", pageno + 1);
        dump_tokens(
            &content,
            &label,
            max_len,
            include_ignorable,
            false,
            true,
            stdout,
        );
    }

    let object_refs = pdf.object_refs();
    for obj_ref in object_refs {
        // Clone only the (small) stream dictionary to classify it, rather
        // than pdf.resolve()'s owned Object — which would deep-clone every
        // stream's raw data just to read /Type, for every stream in the
        // file (fonts and images included, not just the rare ObjStm).
        let dict = match pdf.resolve_borrowed(obj_ref).map_err(|e| e.to_string())? {
            Object::Stream(stream) => stream.dict.clone(),
            _ => continue,
        };
        if !resolve_objstm_type(&mut pdf, &dict) {
            continue;
        }
        let Object::Stream(stream) = pdf.resolve_borrowed(obj_ref).map_err(|e| e.to_string())?
        else {
            continue;
        };
        let decoded = filters::decode_stream_data_recovering_with_limits(
            &stream.dict,
            &stream.data,
            DecodeLimits {
                max_output: None,
                max_filter_chain: None,
            },
        )
        .map_err(|e| e.to_string())?;
        report_stream_events(&decoded.events, stderr);
        let label = format!("OBJECT STREAM {}", obj_ref.number);
        dump_tokens(
            &decoded.data,
            &label,
            max_len,
            include_ignorable,
            false,
            false,
            stdout,
        );
    }

    Ok(())
}

fn resolve_objstm_type(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>, dict: &flpdf::Dictionary) -> bool {
    let Some(type_val) = dict.get(b"Type") else {
        return false;
    };
    // /Type may be reached through a holder chain of two or more indirect
    // references, not just one hop; resolve_ref_chain follows it to the
    // terminal value the same way every other flpdf consumer does.
    match flpdf::ref_chain::resolve_ref_chain(pdf, type_val) {
        Ok((Object::Name(n), _)) => n == b"ObjStm",
        _ => false,
    }
}

// qpdf's test_tokenizer.cc prints nothing of the kind; these diagnostics are
// an flpdf-qtest-tools addition for visibility into a recovering decode. They
// go to stderr, not stdout, so they never pollute the token dump that is
// compared against qpdf's stdout.
fn report_stream_events(events: &[StreamDecodeEvent], stderr: &mut dyn io::Write) {
    for event in events {
        match event {
            StreamDecodeEvent::Warning(w) => {
                let _ = writeln!(stderr, "WARNING: {} (code {})", w.message, w.code);
            }
            StreamDecodeEvent::Error(e) => {
                let _ = writeln!(stderr, "ERROR: {e}");
            }
            StreamDecodeEvent::Data(_) => {}
        }
    }
}

fn dump_tokens(
    input: &[u8],
    label: &str,
    max_len: usize,
    include_ignorable: bool,
    skip_streams: bool,
    skip_inline_images: bool,
    stdout: &mut dyn io::Write,
) {
    let _ = writeln!(stdout, "--- BEGIN {label} ---");

    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();
    if include_ignorable {
        tokenizer.include_ignorable();
    }

    let mut done = false;
    let mut inline_image_offset = None;

    while !done {
        if inline_image_offset.is_some() {
            if let Err(_e) = tokenizer.expect_inline_image() {
                let _ = writeln!(stdout, "EI not found; resuming normal scanning");
                inline_image_offset = None;
                continue;
            }
        }
        let effective_max_len = if inline_image_offset.is_some() {
            0
        } else {
            max_len
        };
        let token = match tokenizer.read_token(true, effective_max_len) {
            Ok(t) => t,
            Err(e) => {
                let _ = writeln!(stdout, "tokenizer error: {e}");
                break;
            }
        };
        let offset = token.start;

        if inline_image_offset.is_some() && token.token_type == TokenType::Bad {
            let _ = writeln!(stdout, "EI not found; resuming normal scanning");
            tokenizer
                .set_position(inline_image_offset.unwrap())
                .expect("position after ID separator");
            inline_image_offset = None;
            continue;
        }
        inline_image_offset = None;

        let _ = write!(stdout, "{offset}: {}", token_type_name(token.token_type));
        if token.token_type != TokenType::Eof {
            let _ = write!(stdout, ": {}", sanitize(&token.value));
            if token.value != token.raw {
                let _ = write!(stdout, " (raw: {})", sanitize(&token.raw));
            }
        }
        if let Some(ref msg) = token.error_message {
            let _ = write!(stdout, " ({})", sanitize(msg));
        }
        let _ = writeln!(stdout);

        if skip_streams && token.token_type == TokenType::Word && token.value == b"stream" {
            let _ = writeln!(stdout, "skipping to endstream");
            let saved = tokenizer.position();
            if let Some(endstream_start) = find_endstream(input, saved) {
                tokenizer
                    .set_position(endstream_start)
                    .expect("position within input");
            } else {
                let _ = writeln!(stdout, "endstream not found");
                tokenizer
                    .set_position(saved)
                    .expect("position within input");
            }
        } else if skip_inline_images && token.token_type == TokenType::Word && token.value == b"ID"
        {
            // qpdf's is->read(&ch, 1) doesn't check how many bytes it got —
            // it always proceeds to expectInlineImage and records the
            // cursor. A content stream that ends right after `ID` (no
            // separator byte) still enters inline-image recovery, which the
            // next iteration's EOF handling reports as "EI not found".
            let _ = tokenizer.consume_one_byte();
            inline_image_offset = Some(tokenizer.position());
        } else if token.token_type == TokenType::Eof {
            done = true;
        }
    }

    let _ = writeln!(stdout, "--- END {label} ---");
}

// Mirrors qpdf's `InputSource::findFirst` + `Finder::check()` as used by
// `qpdf/test_tokenizer.cc`'s `try_skipping`: for each literal occurrence of
// "endstream", tokenize forward from that exact byte (a fresh, unconfigured
// tokenizer, no relation to what precedes it) and accept only if the result
// is the word token "endstream". Unlike a delimiter-boundary scan, this does
// not require a delimiter before the match -- streams that lack a newline
// before `endstream` still match starting mid-data. On success the return
// value is the start of "endstream" itself, not the position after it,
// since qpdf leaves the input positioned there for the next token read.
fn find_endstream(input: &[u8], start: usize) -> Option<usize> {
    let search = &input[start..];
    let mut pos = 0;
    while pos < search.len() {
        let found = search[pos..].windows(9).position(|w| w == b"endstream")?;
        let abs = start + pos + found;
        let mut probe = Tokenizer::new(&input[abs..]);
        if let Ok(token) = probe.read_token(true, 0) {
            if token.token_type == TokenType::Word && token.value == b"endstream" {
                return Some(abs);
            }
        }
        pos += found + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::filters::StreamDecodeWarning;
    use flpdf::{Dictionary, Error, ObjectRef};

    fn open_minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let bytes: &[u8] = b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
            xref\n0 3\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n\
            trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n110\n%%EOF\n";
        Pdf::open(std::io::Cursor::new(bytes.to_vec())).expect("parse minimal PDF")
    }

    #[test]
    fn resolve_objstm_type_true_for_direct_name() {
        let mut pdf = open_minimal_pdf();
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name(b"ObjStm".to_vec()));
        assert!(resolve_objstm_type(&mut pdf, &dict));
    }

    #[test]
    fn resolve_objstm_type_true_for_single_hop_reference() {
        let mut pdf = open_minimal_pdf();
        pdf.set_object(ObjectRef::new(100, 0), Object::Name(b"ObjStm".to_vec()));
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Reference(ObjectRef::new(100, 0)));
        assert!(resolve_objstm_type(&mut pdf, &dict));
    }

    #[test]
    fn resolve_objstm_type_true_for_two_hop_reference_chain() {
        // A bare top-level object body of "N G R" parses as an Integer, not
        // a Reference (qpdf does the same), so this exact holder chain
        // (100 -> 101 -> /ObjStm) cannot arise from parsing raw PDF bytes.
        // pdf.set_object constructs it directly to exercise the full
        // resolve_ref_chain contract defensively, matching how every other
        // flpdf consumer of that shared primitive is expected to behave.
        let mut pdf = open_minimal_pdf();
        pdf.set_object(
            ObjectRef::new(100, 0),
            Object::Reference(ObjectRef::new(101, 0)),
        );
        pdf.set_object(ObjectRef::new(101, 0), Object::Name(b"ObjStm".to_vec()));
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Reference(ObjectRef::new(100, 0)));
        assert!(resolve_objstm_type(&mut pdf, &dict));
    }

    #[test]
    fn resolve_objstm_type_false_for_other_name_or_missing_type() {
        let mut pdf = open_minimal_pdf();
        let mut other = Dictionary::new();
        other.insert("Type", Object::Name(b"XRef".to_vec()));
        assert!(!resolve_objstm_type(&mut pdf, &other));
        assert!(!resolve_objstm_type(&mut pdf, &Dictionary::new()));
    }

    #[test]
    fn find_endstream_finds_word_at_start() {
        let data = b"endstream ";
        assert_eq!(find_endstream(data, 0), Some(0));
    }

    #[test]
    fn find_endstream_matches_without_preceding_delimiter() {
        // qpdf's Finder tokenizes forward from the match; it never looks at
        // the byte before "endstream", so data glued directly onto it (no
        // newline before endstream) still matches.
        let data = b"xendstream ";
        assert_eq!(find_endstream(data, 0), Some(1));
    }

    #[test]
    fn find_endstream_rejects_when_word_extends_past_match() {
        let data = b" endstreamx";
        assert_eq!(find_endstream(data, 0), None);
    }

    #[test]
    fn find_endstream_at_crlf_boundary() {
        let data = b" stream\r\nendstream\r";
        assert_eq!(find_endstream(data, 0), Some(9));
    }

    #[test]
    fn find_endstream_with_nul_boundaries() {
        let data = b"\x00endstream\x00";
        assert_eq!(find_endstream(data, 0), Some(1));
    }

    #[test]
    fn find_endstream_returns_none_when_absent() {
        let data = b"no match here";
        assert_eq!(find_endstream(data, 0), None);
    }

    #[test]
    fn report_stream_events_writes_warnings_and_errors_to_stderr() {
        let events = vec![
            StreamDecodeEvent::Warning(StreamDecodeWarning {
                message: "truncated stream".into(),
                code: -5,
            }),
            StreamDecodeEvent::Error(Error::parse(0, "boom")),
            StreamDecodeEvent::Data(vec![1, 2, 3]),
        ];
        let mut stderr = Vec::new();
        report_stream_events(&events, &mut stderr);
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("WARNING: truncated stream (code -5)"));
        assert!(stderr.contains("boom"));
    }

    #[test]
    fn sanitize_printable_passes_through() {
        assert_eq!(sanitize(b"hello"), "hello");
    }

    #[test]
    fn sanitize_non_printable_escapes() {
        assert_eq!(sanitize(&[0x00]), "\\x00");
        assert_eq!(sanitize(&[0x7f]), "\\x7f");
        assert_eq!(sanitize(&[0xff]), "\\xff");
    }
}
