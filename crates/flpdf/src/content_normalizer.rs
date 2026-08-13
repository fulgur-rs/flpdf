//! Mirrors qpdf 11.9.0 libqpdf/ContentNormalizer.cc.

use crate::{
    pipeline::{
        buffer::Buffer, qpdf_tokenizer::QpdfTokenizer, Pipeline, PipelineRef, PipelineResult,
    },
    token_filter::{TokenFilter, TokenFilterOutput},
    tokenizer::{Token, TokenType},
};

/// Holds normalized content-stream bytes and qpdf-compatible bad-token state.
///
/// Values are produced by [`normalize_content_stream`]. The status accessors
/// allow callers to distinguish clean normalization from best-effort output
/// that contains malformed PDF tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentNormalization {
    bytes: Vec<u8>,
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

impl ContentNormalization {
    /// Borrows the normalized output bytes without consuming this result.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consumes this result and returns its normalized output allocation.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Reports whether normalization encountered at least one bad token.
    #[must_use]
    pub fn any_bad_tokens(&self) -> bool {
        self.any_bad_tokens
    }

    /// Reports whether the final non-EOF token was bad.
    ///
    /// This mirrors qpdf's `lastTokenWasBad` state and indicates that
    /// coalescing adjacent page-content streams may recover a token split
    /// across a stream boundary.
    #[must_use]
    pub fn last_token_was_bad(&self) -> bool {
        self.last_token_was_bad
    }
}

#[derive(Default)]
struct ContentNormalizer {
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

/// The qpdf output normalizer as an owned pipeline stage.
///
/// `Pl_QPDFTokenizer` keeps a filter instance alive beside its stage in qpdf.
/// Rust keeps the same relationship inside this stage: the tokenizer is
/// created for the accumulated input during `finish`, while the normalizer
/// state and downstream ownership live in one value. This is the output-side
/// stage used by `ObjectHandle::pipe_stream_data`; the public helper below
/// remains the small whole-buffer convenience API.
#[allow(dead_code)]
pub(crate) struct ContentNormalizerPipeline<'a> {
    identifier: String,
    next: PipelineRef<'a>,
    normalizer: ContentNormalizer,
    data: Vec<u8>,
    warning_callback: Option<NormalizerWarningCallback>,
    finished: bool,
}

type NormalizerWarningCallback = Box<dyn FnMut(&str) -> PipelineResult<()> + 'static>;

#[allow(dead_code, clippy::type_complexity)]
impl<'a> ContentNormalizerPipeline<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: impl Into<PipelineRef<'a>>) -> Self {
        Self {
            identifier: identifier.into(),
            next: next.into(),
            normalizer: ContentNormalizer::default(),
            data: Vec::new(),
            warning_callback: None,
            finished: false,
        }
    }

    pub(crate) fn set_warning_callback(&mut self, callback: NormalizerWarningCallback) {
        self.warning_callback = Some(callback);
    }

    fn report_warnings(&mut self) -> PipelineResult<()> {
        if !self.normalizer.any_bad_tokens {
            return Ok(());
        }
        let Some(callback) = self.warning_callback.as_mut() else {
            return Ok(());
        };
        callback("content normalization encountered bad tokens")?;
        if self.normalizer.last_token_was_bad {
            callback(
                "normalized content ended with a bad token; you may be able to resolve this by coalescing content streams in combination with normalizing content. From the command line, specify --coalesce-contents",
            )?;
        } // cov:ignore: control-flow marker; the warning branch above is exercised by the owned pipeline test
        callback(
            "Resulting stream data may be corrupted but is may still useful for manual inspection. For more information on this warning, search for content normalization in the manual.",
        )
    }
}

impl Pipeline for ContentNormalizerPipeline<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.data.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let input = std::mem::take(&mut self.data);
        let mut tokenizer = QpdfTokenizer::new(
            self.identifier.clone(),
            &mut self.normalizer,
            Some(&mut self.next),
        );
        tokenizer.write(&input)?;
        tokenizer.finish()?;
        drop(tokenizer);
        self.report_warnings()
    }
}

impl ContentNormalizer {
    fn write_space(
        &mut self,
        raw: &[u8],
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        for (index, &byte) in raw.iter().enumerate() {
            if byte == b'\r' {
                if raw.get(index + 1) != Some(&b'\n') {
                    output.write(b"\n")?;
                }
            } else {
                output.write(std::slice::from_ref(&byte))?;
            }
        }
        Ok(())
    }

    fn finish(self, bytes: Vec<u8>) -> ContentNormalization {
        ContentNormalization {
            bytes,
            any_bad_tokens: self.any_bad_tokens,
            last_token_was_bad: self.last_token_was_bad,
        }
    }
}

impl TokenFilter for ContentNormalizer {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        if token.token_type == TokenType::Bad {
            self.any_bad_tokens = true;
            self.last_token_was_bad = true;
        } else if token.token_type != TokenType::Eof {
            self.last_token_was_bad = false;
        }

        match token.token_type {
            TokenType::Space => self.write_space(&token.raw, output)?,
            TokenType::String | TokenType::Name => {
                output.write_token(&Token::new(token.token_type, token.value.clone()))?;
            }
            _ => output.write_token(token)?,
        }

        if matches!(token.token_type, TokenType::String | TokenType::Name)
            && token.raw.iter().any(|byte| matches!(*byte, b'\r' | b'\n'))
        {
            output.write(b"\n")?;
        }
        Ok(())
    }
}

/// Normalizes decoded PDF content-stream bytes using qpdf 11.9.0 token rules.
///
/// The operation is input-infallible: malformed tokens are retained in the
/// returned bytes and reported through [`ContentNormalization::any_bad_tokens`]
/// and [`ContentNormalization::last_token_was_bad`].
///
/// # Examples
///
/// ```
/// use flpdf::normalize_content_stream;
///
/// let normalized = normalize_content_stream(b"q\rQ");
/// assert_eq!(normalized.as_bytes(), b"q\nQ");
/// assert!(!normalized.any_bad_tokens());
/// ```
#[must_use]
pub fn normalize_content_stream(input: &[u8]) -> ContentNormalization {
    let mut output = Buffer::new("content normalizer output", None);
    let mut normalizer = ContentNormalizer::default();
    {
        let mut tokenizer =
            QpdfTokenizer::new("content normalizer", &mut normalizer, Some(&mut output));
        tokenizer
            .write(input)
            .expect("buffer-backed tokenizer write is infallible");
        tokenizer
            .finish()
            .expect("allow-bad tokenizer finish is infallible");
    }
    let bytes = output
        .take_buffer()
        .expect("finished output buffer is ready");
    normalizer.finish(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::buffer::Buffer;
    use crate::pipeline::qpdf_tokenizer::QpdfTokenizer;
    use crate::pipeline::Pipeline;
    use crate::tokenizer::TokenType;
    use std::fmt::Write as _;
    use std::path::Path;
    use std::process::Command;
    use std::{cell::RefCell, rc::Rc};
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

    fn normalizer_oracle_cases() -> [(&'static str, &'static [u8]); 12] {
        [
            ("layout-comments-crlf", b"% keep\r\nBT  /N#61me Q"),
            ("string-control", b"(\x01) Tj"),
            ("string-newline", b"(a\rb) Tj"),
            ("iso-latin-literal", b"<a061626364> Tj"),
            ("iso-latin-hex", b"<a0616263> Tj"),
            ("bad-recovers", b"<0g> q"),
            ("bad-at-eof", b"<0g"),
            ("id-at-eof", b"ID"),
            ("id-crlf-separator", b"BI ID\r\nraw EI Q"),
            ("inline-false-ei", b"BI /W 1 ID one EI A1 two EI Q"),
            ("inline-binary", b"BI /W 1 ID \0\xff EI Q"),
            ("all-space", b"q \t\0\x0c\r\r\n\nQ"),
        ]
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            write!(encoded, "{byte:02x}").unwrap();
        }
        encoded
    }

    fn normalizer_record(input: &[u8]) -> String {
        let result = normalize_content_stream(input);
        format!(
            "output\t{}\nany_bad_tokens\t{}\nlast_token_was_bad\t{}\n",
            hex_encode(result.as_bytes()),
            u8::from(result.any_bad_tokens()),
            u8::from(result.last_token_was_bad()),
        )
    }

    fn run_normalizer_probe_command(
        mut command: Command,
        probe: &Path,
        name: &str,
        input: &[u8],
    ) -> String {
        let output = command
            .args([
                "--mode",
                "normalize",
                "--input-hex",
                &hex_encode(input),
                "--allow-eof",
                "1",
                "--include-ignorable",
                "1",
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
            // cov:ignore-start: the script supplies a verified executable; this is failure-only harness diagnostics
            .unwrap_or_else(|error| {
                panic!(
                    "failed to execute qpdf content normalizer probe {} for {name}: {error}",
                    probe.display()
                )
            });
        // cov:ignore-end
        assert!(
            output.status.success(),
            "qpdf content normalizer probe failed for {name} ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr), // cov:ignore: failure-only assert diagnostic
        );
        String::from_utf8(output.stdout).expect("probe records are ASCII")
    }

    fn run_normalizer_probe(probe: &Path, name: &str, input: &[u8]) -> String {
        run_normalizer_probe_command(Command::new(probe), probe, name, input)
    }

    /// Run a stand-in probe script.
    ///
    /// The script is handed to `/bin/sh` as an argument rather than executed
    /// directly, so a still-open write handle cannot make the spawn fail with
    /// `ETXTBSY`.
    #[cfg(unix)]
    fn run_test_probe(probe: &Path, name: &str, input: &[u8]) -> String {
        let mut command = Command::new("/bin/sh");
        command.arg(probe);
        run_normalizer_probe_command(command, probe, name, input)
    }

    #[test]
    fn preserves_layout_comments_and_only_normalizes_qpdf_token_forms() {
        let result = normalize_content_stream(b"% keep\r\nBT  /N#61me (a\rb) Tj\rQ");
        assert_eq!(result.as_bytes(), b"% keep\nBT  /Name (a\\nb)\n Tj\nQ");
        assert!(!result.any_bad_tokens());
        assert!(!result.last_token_was_bad());
    }

    #[test]
    fn normalizes_every_pdf_space_shape_without_collapsing_it() {
        let result = normalize_content_stream(b"q \t\0\x0c\r\r\n\nQ");
        assert_eq!(result.as_bytes(), b"q \t\0\x0c\n\n\nQ");
    }

    #[test]
    fn bad_token_state_clears_only_after_a_non_eof_good_token() {
        let recovered = normalize_content_stream(b"<0g> q");
        assert_eq!(recovered.as_bytes(), b"<0g> q");
        assert!(recovered.any_bad_tokens());
        assert!(!recovered.last_token_was_bad());

        let consecutive_then_recovered = normalize_content_stream(b")) q");
        assert_eq!(consecutive_then_recovered.as_bytes(), b")) q");
        assert!(consecutive_then_recovered.any_bad_tokens());
        assert!(!consecutive_then_recovered.last_token_was_bad());

        let terminal = normalize_content_stream(b"<0g");
        assert_eq!(terminal.as_bytes(), b"<0g");
        assert!(terminal.any_bad_tokens());
        assert!(terminal.last_token_was_bad());
    }

    #[test]
    fn id_at_eof_injects_default_space_then_reports_bad_inline_image() {
        let result = normalize_content_stream(b"ID");
        assert_eq!(result.as_bytes(), b"ID ");
        assert!(result.any_bad_tokens());
        assert!(result.last_token_was_bad());
    }

    #[test]
    fn id_separator_is_consumed_as_one_synthetic_space_token() {
        for (input, expected) in [
            (b"BI ID raw EI Q".as_slice(), b"BI ID raw EI Q".as_slice()),
            (b"BI ID\traw EI Q".as_slice(), b"BI ID\traw EI Q".as_slice()),
            (b"BI ID\nraw EI Q".as_slice(), b"BI ID\nraw EI Q".as_slice()),
            (b"BI ID\rraw EI Q".as_slice(), b"BI ID\nraw EI Q".as_slice()),
            (
                b"BI ID\r\nraw EI Q".as_slice(),
                b"BI ID\n\nraw EI Q".as_slice(),
            ),
            (b"BI ID\0raw EI Q".as_slice(), b"BI ID\0raw EI Q".as_slice()),
        ] {
            assert_eq!(normalize_content_stream(input).as_bytes(), expected);
        }
    }

    #[test]
    fn inline_image_payload_and_false_ei_candidates_remain_raw() {
        let input = b"BI /W 1 ID \0\xff EI A1 two EI Q";
        let result = normalize_content_stream(input);
        assert_eq!(result.as_bytes(), input);
        assert!(!result.any_bad_tokens());
    }

    #[derive(Default)]
    struct RecordingFilter(Vec<TokenType>);

    impl TokenFilter for RecordingFilter {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            self.0.push(token.token_type);
            output.write_token(token)
        }

        fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            self.0.push(TokenType::BraceOpen);
            Ok(())
        }
    }

    #[test]
    fn shared_pipeline_delivers_eof_token_before_handle_eof() {
        let mut output = Buffer::new("normalized", None);
        let mut filter = RecordingFilter::default();
        {
            let mut tokenizer = QpdfTokenizer::new("normalizer", &mut filter, Some(&mut output));
            tokenizer.write(b"q").unwrap();
            tokenizer.finish().unwrap();
        }
        assert_eq!(
            filter.0,
            vec![TokenType::Word, TokenType::Eof, TokenType::BraceOpen]
        );
    }

    #[test]
    fn owned_pipeline_reports_terminal_bad_tokens_and_finishes_only_once() {
        let mut output = Buffer::new("normalized", None);
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let mut normalizer = ContentNormalizerPipeline::new("owned normalizer", &mut output);
        let warning_sink = Rc::clone(&warnings);
        normalizer.set_warning_callback(Box::new(move |message| {
            warning_sink.borrow_mut().push(message.to_owned());
            Ok(())
        }));

        assert_eq!(normalizer.identifier(), "owned normalizer");
        normalizer.write(b"<0g").unwrap();
        normalizer.finish().unwrap();
        normalizer.finish().unwrap();
        drop(normalizer);

        assert_eq!(output.take_buffer().unwrap(), b"<0g");
        assert_eq!(warnings.borrow().len(), 3);
    }

    #[test]
    fn normalizer_oracle_case_records_match_snapshots() {
        let expected = [
            (
                "layout-comments-crlf",
                "output\t25206b6565700a425420202f4e616d652051\n\
                 any_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "string-control",
                "output\t3c30313e20546a\nany_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "string-newline",
                "output\t28615c6e62290a20546a\nany_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "iso-latin-literal",
                "output\t28a0616263642920546a\nany_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "iso-latin-hex",
                "output\t3c61303631363236333e20546a\n\
                 any_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "bad-recovers",
                "output\t3c30673e2071\nany_bad_tokens\t1\nlast_token_was_bad\t0\n",
            ),
            (
                "bad-at-eof",
                "output\t3c3067\nany_bad_tokens\t1\nlast_token_was_bad\t1\n",
            ),
            (
                "id-at-eof",
                "output\t494420\nany_bad_tokens\t1\nlast_token_was_bad\t1\n",
            ),
            (
                "id-crlf-separator",
                "output\t42492049440a0a7261772045492051\n\
                 any_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "inline-false-ei",
                "output\t4249202f572031204944206f6e652045492041312074776f2045492051\n\
                 any_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "inline-binary",
                "output\t4249202f5720312049442000ff2045492051\n\
                 any_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
            (
                "all-space",
                "output\t712009000c0a0a0a51\nany_bad_tokens\t0\nlast_token_was_bad\t0\n",
            ),
        ];

        for ((name, input), (expected_name, expected_record)) in
            normalizer_oracle_cases().into_iter().zip(expected)
        {
            assert_eq!(name, expected_name);
            assert_eq!(normalizer_record(input), expected_record, "case {name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn normalizer_probe_command_passes_exact_arguments_and_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        fs::write(&probe, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        let mut permissions = fs::metadata(&probe).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&probe, permissions).unwrap();

        let (name, input) = normalizer_oracle_cases()[10];
        assert_eq!(
            run_test_probe(&probe, name, input),
            "--mode\nnormalize\n--input-hex\n4249202f5720312049442000ff2045492051\n\
             --allow-eof\n1\n--include-ignorable\n1\n--allow-bad\n1\n--max-len\n0\n\
             --inline-offset\nnone\n--chunks\nall\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn normalizer_probe_that_is_still_open_for_writing_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("probe");
        fs::write(&probe, "#!/bin/sh\nprintf 'output\\t\\n'\n").unwrap();
        let mut permissions = fs::metadata(&probe).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&probe, permissions).unwrap();
        let _write_open = fs::OpenOptions::new().write(true).open(&probe).unwrap();

        let (name, input) = normalizer_oracle_cases()[10];
        assert_eq!(run_test_probe(&probe, name, input), "output\t\n");
    }

    #[cfg(unix)]
    #[test]
    fn normalizer_probe_wrapper_executes_requested_path() {
        let (name, input) = normalizer_oracle_cases()[10];

        assert_eq!(run_normalizer_probe(Path::new("true"), name, input), "");
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 content normalizer oracle"]
    // cov:ignore-start: ignored live entry point; ordinary tests cover every authored case locally
    fn qpdf_content_normalizer_differential() {
        let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
            .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
        for (name, input) in normalizer_oracle_cases() {
            assert_eq!(
                normalizer_record(input),
                run_normalizer_probe(std::path::Path::new(&probe), name, input),
                "case {name}"
            );
        }
    }
    // cov:ignore-end
}
