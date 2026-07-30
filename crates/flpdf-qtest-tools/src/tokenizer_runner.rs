use std::ffi::OsString;
use std::fmt::Write;
use std::fs;
use std::io;

use flpdf::filters::{self, DecodeLimits};
use flpdf::pages::{page_content_bytes, page_refs};
use flpdf::tokenizer::{TokenType, Tokenizer};
use flpdf::{Object, Pdf, PdfOpenOptions};

/// Outcome produced by the tokenizer runner.
pub enum RunOutcome {
    Exit(u8),
}

/// Run the qpdf `test_tokenizer` contract.
pub fn run(
    args: &[OsString],
    stdout: &mut dyn io::Write,
    stderr: &mut dyn io::Write,
) -> RunOutcome {
    let mut max_len: usize = 0;
    let mut include_ignorable = true;
    let mut filename: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        let arg = args[i].to_string_lossy().into_owned();
        if arg.starts_with('-') {
            if arg == "-maxlen" {
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
            } else if arg == "-no-ignorable" {
                include_ignorable = false;
            } else {
                usage(args, stderr);
                return RunOutcome::Exit(2);
            }
        } else if filename.is_some() {
            usage(args, stderr);
            return RunOutcome::Exit(2);
        } else {
            filename = Some(arg);
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

    match process(&filename, include_ignorable, max_len, stdout) {
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

/// qpdf correspondence: `test_tokenizer.cc:50-94`
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
    filename: &str,
    include_ignorable: bool,
    max_len: usize,
    stdout: &mut dyn io::Write,
) -> Result<(), String> {
    let file_bytes = fs::read(filename).map_err(|e| e.to_string())?;

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
        repair: false,
        ..PdfOpenOptions::default()
    };
    let mut pdf = Pdf::open_mem_owned_with_options(bytes, options).map_err(|e| e.to_string())?;

    let page_refs = page_refs(&mut pdf).map_err(|e| e.to_string())?;
    for (pageno, page_ref) in page_refs.iter().enumerate() {
        let content = page_content_bytes(&mut pdf, *page_ref).map_err(|e| e.to_string())?;
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
            let is_objstm = stream
                .dict
                .get(b"Type")
                .and_then(|v| v.as_name())
                .map(|n| n == b"ObjStm")
                .unwrap_or(false);
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

fn dump_tokens(
    input: &[u8],
    label: &str,
    max_len: usize,
    include_ignorable: bool,
    skip_streams: bool,
    skip_inline_images: bool,
    stdout: &mut dyn io::Write,
) {
    writeln!(stdout, "--- BEGIN {label} ---").unwrap();

    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();
    if include_ignorable {
        tokenizer.include_ignorable();
    }

    let mut done = false;
    let mut inline_image_offset = None;

    while !done {
        if inline_image_offset.is_some() {
            if let Err(e) = tokenizer.expect_inline_image() {
                let _ = writeln!(stdout, "EI not found; resuming normal scanning: {e:?}");
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

        write!(stdout, "{offset}: {}", token_type_name(token.token_type)).unwrap();
        if token.token_type != TokenType::Eof {
            write!(stdout, ": {}", sanitize(&token.value)).unwrap();
            if token.value != token.raw {
                write!(stdout, " (raw: {})", sanitize(&token.raw)).unwrap();
            }
        }
        if let Some(ref msg) = token.error_message {
            write!(stdout, " ({})", String::from_utf8_lossy(msg)).unwrap();
        }
        writeln!(stdout).unwrap();

        if skip_streams && token.token_type == TokenType::Word && token.value == b"stream" {
            writeln!(stdout, "skipping to endstream").unwrap();
            let saved = tokenizer.position();
            if let Some(after_endstream) = find_endstream(input, saved) {
                tokenizer
                    .set_position(after_endstream)
                    .expect("position within input");
            } else {
                writeln!(stdout, "endstream not found").unwrap();
                tokenizer
                    .set_position(saved)
                    .expect("position within input");
            }
        } else if skip_inline_images && token.token_type == TokenType::Word && token.value == b"ID"
        {
            tokenizer.consume_one_byte().expect("byte after ID");
            inline_image_offset = Some(tokenizer.position());
        } else if token.token_type == TokenType::Eof {
            done = true;
        }
    }

    writeln!(stdout, "--- END {label} ---").unwrap();
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\n'
            | b'\r'
            | b'\t'
            | b'\x0c'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

fn find_endstream(input: &[u8], start: usize) -> Option<usize> {
    let search = &input[start..];
    let mut pos = 0;
    while pos < search.len() {
        let found = search[pos..].windows(9).position(|w| w == b"endstream")?;
        let abs = start + pos + found;
        let is_start = abs == 0 || is_delimiter(input[abs - 1]);
        let after = abs + 9;
        let is_end = after >= input.len() || is_delimiter(input[after]);
        if is_start && is_end {
            return Some(after);
        }
        pos += found + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_endstream_finds_at_start() {
        let data = b"endstream ";
        assert_eq!(find_endstream(data, 0), Some(9));
    }

    #[test]
    fn find_endstream_requires_delimiter_before() {
        let data = b"xendstream ";
        assert_eq!(find_endstream(data, 0), None);
    }

    #[test]
    fn find_endstream_requires_delimiter_after() {
        let data = b" endstreamx";
        assert_eq!(find_endstream(data, 0), None);
    }

    #[test]
    fn find_endstream_at_delimiter_boundaries() {
        let data = b" stream\r\nendstream\r";
        assert_eq!(find_endstream(data, 0), Some(18));
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
