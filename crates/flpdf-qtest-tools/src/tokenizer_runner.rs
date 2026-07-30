use std::ffi::{OsStr, OsString};
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;

use flpdf::filters::{self, DecodeLimits, StreamDecodeEvent};
use flpdf::pages::{page_content_bytes, page_refs};
use flpdf::tokenizer::{TokenType, Tokenizer};
use flpdf::{Object, Pdf, PdfOpenOptions};

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
                    usage(args, stderr);
                    return RunOutcome::Exit(2);
                }
                max_len = match args[i].to_string_lossy().parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        usage(args, stderr);
                        return RunOutcome::Exit(2);
                    }
                };
            } else if arg_str == "-no-ignorable" {
                include_ignorable = false;
            } else {
                usage(args, stderr);
                return RunOutcome::Exit(2);
            }
        } else if filename.is_some() {
            usage(args, stderr);
            return RunOutcome::Exit(2);
        } else {
            filename = Some(args[i].clone());
        }
        i += 1;
    }

    let filename = match filename {
        Some(f) => f,
        None => {
            usage(args, stderr);
            return RunOutcome::Exit(2);
        }
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

fn usage(args: &[OsString], stderr: &mut dyn io::Write) {
    let name = program_name(args);
    let _ = writeln!(
        stderr,
        "Usage: {} [-maxlen len | -no-ignorable] filename",
        String::from_utf8_lossy(&name)
    );
}

fn program_name(args: &[OsString]) -> Vec<u8> {
    args.first()
        .and_then(|a| a.to_str())
        .map(|s| {
            s.rfind('/')
                .map(|i| s.as_bytes()[i + 1..].to_vec())
                .unwrap_or_else(|| s.as_bytes().to_vec())
        })
        .unwrap_or_else(|| b"test_tokenizer".to_vec())
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

fn token_type_name(token_type: TokenType) -> &'static str {
    match token_type {
        TokenType::Bad => "bad",
        TokenType::ArrayClose => "array_close",
        TokenType::ArrayOpen => "array_open",
        TokenType::BraceClose => "brace_close",
        TokenType::BraceOpen => "brace_open",
        TokenType::DictClose => "dict_close",
        TokenType::DictOpen => "dict_open",
        TokenType::Integer => "integer",
        TokenType::Name => "name",
        TokenType::Real => "real",
        TokenType::String => "string",
        TokenType::Null => "null",
        TokenType::Bool => "bool",
        TokenType::Word => "word",
        TokenType::Eof => "eof",
        TokenType::Space => "space",
        TokenType::Comment => "comment",
        TokenType::InlineImage => "inline-image",
    }
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

    let bytes = file_bytes;
    let options = PdfOpenOptions {
        repair: true,
        allow_weak_crypto: true,
        ..PdfOpenOptions::default()
    };
    let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).map_err(|e| e.to_string())?;

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
        let obj = pdf.resolve(obj_ref).map_err(|e| e.to_string())?;
        if let Object::Stream(ref stream) = obj {
            let is_objstm = resolve_objstm_type(&mut pdf, &stream.dict);
            if is_objstm {
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
        }
    }

    Ok(())
}

fn resolve_objstm_type(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>, dict: &flpdf::Dictionary) -> bool {
    match dict.get(b"Type") {
        Some(Object::Name(n)) => n == b"ObjStm",
        Some(Object::Reference(r)) => match pdf.resolve(*r) {
            Ok(Object::Name(ref n)) => n == b"ObjStm",
            _ => false,
        },
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
        let token = tokenizer.read_token(true, effective_max_len);
        let (token, offset) = match token {
            Ok(t) => {
                let offset = t.start;
                (t, offset)
            }
            Err(e) => {
                let _ = writeln!(stdout, "tokenizer error: {e}");
                break;
            }
        };

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
            if let Err(_e) = tokenizer.consume_one_byte() {
                continue;
            }
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
    use flpdf::Error;

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
