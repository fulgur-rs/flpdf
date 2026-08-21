use std::ffi::{OsStr, OsString};
use std::fmt::Write;
use std::fs;
use std::io::{self, Read, Seek};
use std::path::Path;

use flpdf::pages::repair::prepare_for_optimization;
use flpdf::tokenizer::{TokenType, Tokenizer};
use flpdf::{
    DecodeLevel, Error, ObjectHandle, ObjectRef, Pdf, PdfOpenOptions, Pipeline, PipelineResult,
    Result as FlpdfResult,
};

use crate::common::test_driver_program_name_bytes;
use crate::driver::{emit_new_diagnostics, os_str_diagnostic_bytes, write_warning};

pub enum RunOutcome {
    Exit(u8),
}

#[derive(Default)]
struct ObjectStreamPipeline {
    bytes: Vec<u8>,
}

impl Pipeline for ObjectStreamPipeline {
    fn identifier(&self) -> &str {
        "object stream data"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
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
                max_len = match parse_maxlen(&os_str_diagnostic_bytes(&args[i])) {
                    Some(n) => n,
                    None => return usage_error(args, stderr),
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
            // argv[0] may contain non-UTF-8 bytes on Unix; write it verbatim
            // rather than losing them through a lossy String conversion.
            let _ = stderr.write_all(&program_name(args));
            let _ = write!(stderr, ": exception: {e}");
            RunOutcome::Exit(2)
        }
    }
}

fn usage_error(args: &[OsString], stderr: &mut dyn io::Write) -> RunOutcome {
    usage(args, stderr);
    RunOutcome::Exit(2)
}

fn usage(args: &[OsString], stderr: &mut dyn io::Write) {
    let _ = write!(stderr, "Usage: ");
    let _ = stderr.write_all(&program_name(args));
    let _ = writeln!(stderr, " [-maxlen len | -no-ignorable] filename");
}

// qpdf's test_tokenizer.cc parses -maxlen via `QUtil::string_to_uint`, which skips
// leading whitespace, rejects a leading '-' (throws, uncaught -- unlike this
// helper, which falls back to usage() rather than aborting), then runs the
// argument through `strtoull`: a decimal prefix is consumed and anything after
// the last digit is ignored, and a value with no digits at all converts to 0
// rather than erroring. This mirrors that prefix/range behavior instead of
// `str::parse`'s all-or-nothing conversion.
fn parse_maxlen(input: &[u8]) -> Option<usize> {
    let mut idx = 0;
    while input.get(idx).is_some_and(u8::is_ascii_whitespace) {
        idx += 1;
    }
    if input.get(idx) == Some(&b'-') {
        return None;
    }
    if input.get(idx) == Some(&b'+') {
        idx += 1;
    }
    let mut value: u64 = 0;
    while let Some(digit) = input
        .get(idx)
        .and_then(|byte| byte.checked_sub(b'0'))
        .filter(|digit| *digit <= 9)
    {
        value = value.saturating_mul(10).saturating_add(u64::from(digit));
        idx += 1;
    }
    Some(usize::try_from(value).unwrap_or(usize::MAX))
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

// qpdf keeps this mapping in qpdf/test_tokenizer.cc itself, not in
// libqpdf's QPDFTokenizer -- it is display formatting for this test
// binary, not tokenizer behavior, so it lives here rather than in flpdf.
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
    let filename_diagnostic = os_str_diagnostic_bytes(filename);
    let file_bytes = match fs::read(Path::new(filename)) {
        Ok(bytes) => bytes,
        Err(e) => {
            let crt_message = crate::driver::crt_open_error_message(filename);
            let message =
                crate::driver::open_error_bytes(&filename_diagnostic, crt_message.as_deref(), &e);
            return Err(String::from_utf8_lossy(&message).into_owned());
        }
    };

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
        // This runner formats diagnostics through its explicit output
        // writers; keep the document logger from duplicating them on the
        // process stderr.
        suppress_warnings: true,
        ..PdfOpenOptions::default()
    };
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

    // qpdf's test_tokenizer.cc enumerates pages via
    // `QPDFPageDocumentHelper(qpdf).getAllPages()`, which is `QPDF::getAllPages()`
    // (QPDF_pages.cc) directly -- the tree-repairing walk that clones duplicate
    // leaves, mints direct leaves to indirect objects, and corrects mistyped
    // nodes. `page_refs` performs no such repair and would silently drop or
    // dedup the pages qpdf reports; `prepare_for_optimization` is flpdf's port
    // of that same repair+enumerate walk.
    let page_refs = prepare_for_optimization(&mut pdf)
        .map_err(|e| e.to_string())?
        .map(|prepared| prepared.pages)
        .unwrap_or_default();
    for (pageno, page_ref) in page_refs.iter().enumerate() {
        let content =
            canonical_page_content_bytes(&mut pdf, *page_ref).map_err(|e| e.to_string())?;
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
    let _ = emit_new_diagnostics(
        &pdf,
        &mut diagnostics_written,
        &filename_diagnostic,
        stdout,
        stderr,
    );

    let object_refs = pdf.object_refs();
    for obj_ref in object_refs {
        // Page content is already resolved through the canonical handle path
        // above. Reusing that state is important for damaged content streams:
        // resolving the same object through the legacy compatibility path and
        // then through the canonical path would replay qpdf's warnings.
        let stream_handle = pdf.get_object_handle(obj_ref);
        if !stream_handle.is_resolved() {
            pdf.resolve_object_handle(&stream_handle)
                .map_err(|e| e.to_string())?;
        }
        let Some(stream_dict) = stream_handle.as_stream_dict() else {
            continue;
        };
        if !resolve_objstm_type(&mut pdf, &stream_dict) {
            continue;
        }
        // qpdf's test_tokenizer uses the public pipeStreamData overload so an
        // unfilterable object stream can still be tokenized from raw bytes
        // after its warning. Do not use getStreamData here: qpdf deliberately
        // rejects raw pass-through from that decoded-data API. Keep the
        // canonical handle and avoid Pdf::resolve_borrowed so a stream-length
        // recovery emits only one warning sequence.
        let mut sink = ObjectStreamPipeline::default();
        let mut filtering_attempted = false;
        if !stream_handle
            .pipe_stream_data(
                &mut sink,
                &mut filtering_attempted,
                0,
                DecodeLevel::Specialized,
                false,
                false,
            )
            .map_err(|e| e.to_string())?
        {
            let _ = emit_new_diagnostics(
                &pdf,
                &mut diagnostics_written,
                &filename_diagnostic,
                stdout,
                stderr,
            );
            return Err("error getting decoded stream data".to_owned());
        }
        let decoded = sink.bytes;
        let _ = emit_new_diagnostics(
            &pdf,
            &mut diagnostics_written,
            &filename_diagnostic,
            stdout,
            stderr,
        );
        let label = format!("OBJECT STREAM {}", obj_ref.number);
        dump_tokens(
            &decoded,
            &label,
            max_len,
            include_ignorable,
            false,
            false,
            stdout,
        );
    }
    let _ = emit_new_diagnostics(
        &pdf,
        &mut diagnostics_written,
        &filename_diagnostic,
        stdout,
        stderr,
    );

    Ok(())
}

/// Pipe page contents through canonical [`ObjectHandle`]s before tokenizing.
///
/// qpdf's `QPDFPageObjectHelper::pipeContents` resolves the page's content
/// holders and stream framing through the same canonical object cache later
/// walked by `getAllObjects`. Keeping that order here means a malformed page
/// stream is not parsed once by the legacy `page_content_bytes` compatibility
/// API and a second time while classifying object streams.
fn canonical_page_content_bytes<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> FlpdfResult<Vec<u8>> {
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page)?;
    if !page.has_key(b"/Contents") {
        return Ok(Vec::new());
    }

    let contents = page.get_key(b"/Contents");
    let contents_was_indirect = contents.object_ref().is_some();
    let (contents, _) = pdf.resolve_object_handle_to_terminal_ref(&contents)?;
    if contents.is_null() {
        return Ok(Vec::new());
    }
    if contents_was_indirect && contents.as_array().is_none() && contents.as_stream_dict().is_none()
    {
        // qpdf's page helper warns and continues when a repaired indirect
        // /Contents holder resolves to a non-stream scalar. Keep this narrow
        // recovery boundary; a direct malformed scalar remains an error so
        // callers do not regain the old blanket `unwrap_or_default()` path.
        return Ok(Vec::new());
    }
    let mut streams = Vec::new();
    collect_canonical_content_streams(pdf, &contents, page_ref, &mut streams, true)?;

    let mut output = Vec::new();
    let mut need_newline = false;
    for stream in streams {
        let decoded = stream.get_stream_data(DecodeLevel::Specialized)?;
        if need_newline {
            output.push(b'\n');
        }
        need_newline = decoded.last().copied().unwrap_or(0) != b'\n';
        output.extend_from_slice(decoded.as_ref());
    }
    Ok(output)
}

fn collect_canonical_content_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &ObjectHandle,
    page_ref: ObjectRef,
    streams: &mut Vec<ObjectHandle>,
    allow_array: bool,
) -> FlpdfResult<()> {
    let (value, _) = pdf.resolve_object_handle_to_terminal_ref(value)?;
    if value.as_stream_dict().is_some() {
        streams.push(value);
        return Ok(());
    }

    if let Some(items) = value.as_array() {
        // qpdf's arrayOrStreamToStreamArray accepts only the top-level array
        // and ignores every array element that is not a stream
        // (`QPDFObjectHandle.cc:1428-1469`). In particular, do not recurse
        // into a nested array: that both diverges on self-references and
        // fabricates content that qpdf does not pipe.
        if !allow_array {
            return Ok(());
        }
        for item in items {
            collect_canonical_content_streams(pdf, &item, page_ref, streams, false)?;
        }
        return Ok(());
    }

    // A scalar /Contents value is still an invalid page-level shape, but a
    // non-stream element inside the top-level array is qpdf's ignorable case.
    if !allow_array {
        return Ok(());
    }
    Err(Error::Unsupported(format!(
        "/Contents on page {page_ref} is not a stream or array"
    )))
}

fn resolve_objstm_type(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>, dict: &ObjectHandle) -> bool {
    let type_handle = dict.get_key(b"/Type");
    // qpdf's getKey()/isName() dereference through the canonical object
    // handle. Follow the complete holder chain here as well, while keeping
    // the decode boundary below in its existing Dictionary-shaped form.
    let Ok((type_handle, _)) = pdf.resolve_object_handle_to_terminal_ref(&type_handle) else {
        return false;
    };
    matches!(type_handle.as_name(), Some(name) if name.as_slice() == b"ObjStm")
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
    use flpdf::{Object, ObjectHandle, ObjectRef};

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
        let dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"ObjStm".to_vec()),
        )]);
        assert!(resolve_objstm_type(&mut pdf, &dict));
    }

    #[test]
    fn resolve_objstm_type_true_for_single_hop_reference() {
        let mut pdf = open_minimal_pdf();
        pdf.set_object(ObjectRef::new(100, 0), Object::Name(b"ObjStm".to_vec()));
        let dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            pdf.get_object_handle(ObjectRef::new(100, 0)),
        )]);
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
        let dict = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            pdf.get_object_handle(ObjectRef::new(100, 0)),
        )]);
        assert!(resolve_objstm_type(&mut pdf, &dict));
    }

    #[test]
    fn resolve_objstm_type_false_for_other_name_or_missing_type() {
        let mut pdf = open_minimal_pdf();
        let other = ObjectHandle::dictionary(vec![(
            b"Type".to_vec(),
            ObjectHandle::name(b"XRef".to_vec()),
        )]);
        assert!(!resolve_objstm_type(&mut pdf, &other));
        assert!(!resolve_objstm_type(
            &mut pdf,
            &ObjectHandle::dictionary(Vec::new())
        ));
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
    fn sanitize_printable_passes_through() {
        assert_eq!(sanitize(b"hello"), "hello");
    }

    #[test]
    fn sanitize_non_printable_escapes() {
        assert_eq!(sanitize(&[0x00]), "\\x00");
        assert_eq!(sanitize(&[0x7f]), "\\x7f");
        assert_eq!(sanitize(&[0xff]), "\\xff");
    }

    #[test]
    fn parse_maxlen_plain_decimal() {
        assert_eq!(parse_maxlen(b"5"), Some(5));
    }

    #[test]
    fn parse_maxlen_consumes_decimal_prefix_and_ignores_trailing_garbage() {
        assert_eq!(parse_maxlen(b"5trailing"), Some(5));
    }

    #[test]
    fn parse_maxlen_skips_leading_whitespace() {
        assert_eq!(parse_maxlen(b"  5"), Some(5));
    }

    #[test]
    fn parse_maxlen_with_no_digits_at_all_is_zero() {
        assert_eq!(parse_maxlen(b"abc"), Some(0));
        assert_eq!(parse_maxlen(b""), Some(0));
    }

    #[test]
    fn parse_maxlen_rejects_leading_minus() {
        // QUtil::string_to_uint throws on a leading '-' rather than wrapping;
        // this helper reports the rejection as None so the caller can fall
        // back to usage() instead of matching qpdf's uncaught-exception abort.
        assert_eq!(parse_maxlen(b"-5"), None);
    }

    #[test]
    fn parse_maxlen_accepts_leading_plus() {
        assert_eq!(parse_maxlen(b"+5"), Some(5));
    }

    #[test]
    fn parse_maxlen_saturates_on_overflow() {
        assert_eq!(
            parse_maxlen(b"99999999999999999999999999"),
            Some(usize::MAX)
        );
    }
}
