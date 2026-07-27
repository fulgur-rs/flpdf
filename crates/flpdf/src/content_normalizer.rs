//! Mirrors qpdf 11.9.0 libqpdf/Pl_QPDFTokenizer.cc,
//! libqpdf/ContentNormalizer.cc.

use crate::tokenizer::{Token, TokenType, Tokenizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentNormalization {
    bytes: Vec<u8>,
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

impl ContentNormalization {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    #[must_use]
    pub fn any_bad_tokens(&self) -> bool {
        self.any_bad_tokens
    }

    #[must_use]
    pub fn last_token_was_bad(&self) -> bool {
        self.last_token_was_bad
    }
}

trait TokenFilter {
    fn handle_token(&mut self, token: &Token);
    fn handle_eof(&mut self);
}

fn run_token_filter(input: &[u8], filter: &mut impl TokenFilter) {
    let mut tokenizer = Tokenizer::new(input);
    tokenizer.allow_eof();
    tokenizer.include_ignorable();

    loop {
        let token = tokenizer
            .read_token(true, 0)
            .expect("allow-bad qpdf tokenization is input-infallible");
        let is_eof = token.token_type == TokenType::Eof;
        let is_id = token.is_word_value(b"ID");
        filter.handle_token(&token);
        if is_eof {
            break;
        }
        if is_id {
            let separator = tokenizer.consume_one_byte_or(b' ');
            filter.handle_token(&Token::new(TokenType::Space, vec![separator]));
            tokenizer
                .expect_inline_image()
                .expect("ID handling leaves the tokenizer between tokens");
        }
    }
    filter.handle_eof();
}

#[derive(Default)]
struct ContentNormalizer {
    output: Vec<u8>,
    any_bad_tokens: bool,
    last_token_was_bad: bool,
}

impl ContentNormalizer {
    fn write_space(&mut self, raw: &[u8]) {
        for (index, &byte) in raw.iter().enumerate() {
            if byte == b'\r' {
                if raw.get(index + 1) != Some(&b'\n') {
                    self.output.push(b'\n');
                }
            } else {
                self.output.push(byte);
            }
        }
    }

    fn finish(self) -> ContentNormalization {
        ContentNormalization {
            bytes: self.output,
            any_bad_tokens: self.any_bad_tokens,
            last_token_was_bad: self.last_token_was_bad,
        }
    }
}

impl TokenFilter for ContentNormalizer {
    fn handle_token(&mut self, token: &Token) {
        if token.token_type == TokenType::Bad {
            self.any_bad_tokens = true;
            self.last_token_was_bad = true;
        } else if token.token_type != TokenType::Eof {
            self.last_token_was_bad = false;
        }

        match token.token_type {
            TokenType::Space => self.write_space(&token.raw),
            TokenType::String | TokenType::Name => {
                let canonical = Token::new(token.token_type, token.value.clone());
                self.output.extend_from_slice(&canonical.raw);
            }
            _ => self.output.extend_from_slice(&token.raw),
        }

        if matches!(token.token_type, TokenType::String | TokenType::Name)
            && token.raw.iter().any(|byte| matches!(*byte, b'\r' | b'\n'))
        {
            self.output.push(b'\n');
        }
    }

    fn handle_eof(&mut self) {}
}

#[must_use]
pub fn normalize_content_stream(input: &[u8]) -> ContentNormalization {
    let mut normalizer = ContentNormalizer::default();
    run_token_filter(input, &mut normalizer);
    normalizer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::TokenType;
    use std::fmt::Write as _;
    use std::path::Path;
    use std::process::Command;

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

    fn run_normalizer_probe(probe: &Path, name: &str, input: &[u8]) -> String {
        let output = Command::new(probe)
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
        fn handle_token(&mut self, token: &Token) {
            self.0.push(token.token_type);
        }

        fn handle_eof(&mut self) {
            self.0.push(TokenType::BraceOpen);
        }
    }

    #[test]
    fn runner_delivers_eof_token_before_handle_eof() {
        let mut filter = RecordingFilter::default();
        run_token_filter(b"q", &mut filter);
        assert_eq!(
            filter.0,
            vec![TokenType::Word, TokenType::Eof, TokenType::BraceOpen]
        );
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
