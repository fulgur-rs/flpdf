//! qpdf correspondence: Pl_QPDFTokenizer.cc buffered token-filter pipeline.

use crate::{
    pipeline::{Pipeline, PipelineError, PipelineRef, PipelineResult},
    token_filter::{TokenFilter, TokenFilterOutput},
    tokenizer::{Token, TokenType, Tokenizer},
};
use std::{cell::RefCell, rc::Rc};

enum TokenFilterSource<'a> {
    Borrowed(&'a mut dyn TokenFilter),
    Shared(Rc<RefCell<dyn TokenFilter>>),
}

pub(crate) struct QpdfTokenizer<'a> {
    identifier: String,
    filter: TokenFilterSource<'a>,
    next: Option<PipelineRef<'a>>,
    filter_output_attached: bool,
    data: Vec<u8>,
}

impl<'a> QpdfTokenizer<'a> {
    pub(crate) fn new(
        identifier: impl Into<String>,
        filter: &'a mut dyn TokenFilter,
        next: Option<&'a mut dyn Pipeline>,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            filter: TokenFilterSource::Borrowed(filter),
            filter_output_attached: next.is_some(),
            next: next.map(PipelineRef::Borrowed),
            data: Vec::new(),
        }
    }

    /// Construct a tokenizer that owns a shared qpdf-style token filter and
    /// the downstream pipeline chain. `addTokenFilter` stores callback objects
    /// on the stream and may invoke them during more than one write attempt;
    /// the shared handle therefore lives with this stage rather than behind a
    /// temporary borrow.
    pub(crate) fn new_shared(
        identifier: impl Into<String>,
        filter: Rc<RefCell<dyn TokenFilter>>,
        next: Option<PipelineRef<'a>>,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            filter: TokenFilterSource::Shared(filter),
            filter_output_attached: next.is_some(),
            next,
            data: Vec::new(),
        }
    }

    fn handle_token(&mut self, token: &Token) -> PipelineResult<()> {
        let next = self.filter_output_attached.then(|| {
            self.next.as_mut().expect("attached output has a pipeline") as &mut dyn Pipeline
        });
        let mut output = TokenFilterOutput::new(next);
        match &mut self.filter {
            TokenFilterSource::Borrowed(filter) => filter.handle_token(token, &mut output),
            TokenFilterSource::Shared(filter) => {
                filter.borrow_mut().handle_token(token, &mut output)
            }
        }
    }

    fn handle_eof(&mut self) -> PipelineResult<()> {
        let next = self.filter_output_attached.then(|| {
            self.next.as_mut().expect("attached output has a pipeline") as &mut dyn Pipeline
        });
        let mut output = TokenFilterOutput::new(next);
        let result = match &mut self.filter {
            TokenFilterSource::Borrowed(filter) => filter.handle_eof(&mut output),
            TokenFilterSource::Shared(filter) => filter.borrow_mut().handle_eof(&mut output),
        };
        result?;
        if self.filter_output_attached {
            self.filter_output_attached = false;
        }
        Ok(())
    }
}

impl Pipeline for QpdfTokenizer<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.data.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        let input = std::mem::take(&mut self.data);
        let mut tokenizer = Tokenizer::new(&input);
        tokenizer.allow_eof();
        tokenizer.include_ignorable();

        loop {
            let token = tokenizer
                .read_token(true, 0)
                .map_err(|error| PipelineError::runtime(format!("{}: {error}", self.identifier)))?;
            let is_eof = token.token_type == TokenType::Eof;
            let is_id = token.is_word_value(b"ID");
            self.handle_token(&token)?;
            if is_eof {
                break;
            }
            if is_id {
                let separator = tokenizer.consume_one_byte_or(b' ');
                let space = Token::new(TokenType::Space, vec![separator]);
                self.handle_token(&space)?;
                // cov:ignore-start: after a word ID the tokenizer is necessarily between tokens
                tokenizer.expect_inline_image().map_err(|error| {
                    PipelineError::logic(format!("{}: {error:?}", self.identifier))
                })?;
                // cov:ignore-end
            }
        }
        self.handle_eof()?;
        if let Some(next) = self.next.as_mut() {
            next.finish()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::QpdfTokenizer;
    use crate::{
        pipeline::{Pipeline, PipelineError, PipelineRef, PipelineResult},
        token_filter::{TokenFilter, TokenFilterOutput},
        tokenizer::{Token, TokenType},
    };
    use std::{cell::RefCell, fmt::Write as _, path::Path, process::Command, rc::Rc};

    #[derive(Default)]
    struct RecordingFilter {
        events: Vec<(TokenType, Vec<u8>)>,
        eof_calls: usize,
    }

    impl TokenFilter for RecordingFilter {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            self.events.push((token.token_type, token.raw.clone()));
            output.write_token(token)
        }

        fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            self.eof_calls += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        chunks: Vec<Vec<u8>>,
        finishes: usize,
    }

    // cov:ignore-start: test-only sink identifiers have no behavioral role
    impl Pipeline for RecordingSink {
        fn identifier(&self) -> &str {
            "recording sink"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            Ok(())
        }
    }
    // cov:ignore-end

    struct FinishFailSink;

    // cov:ignore-start: test-only sink identifiers have no behavioral role
    impl Pipeline for FinishFailSink {
        fn identifier(&self) -> &str {
            "finish fail sink"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Err(PipelineError::logic("sink finish failed"))
        }
    }
    // cov:ignore-end

    #[derive(Default)]
    struct FinishFailOnceSink {
        chunks: Vec<Vec<u8>>,
        finishes: usize,
    }

    // cov:ignore-start: test-only sink identifiers have no behavioral role
    impl Pipeline for FinishFailOnceSink {
        fn identifier(&self) -> &str {
            "finish fail once sink"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes += 1;
            if self.finishes == 1 {
                return Err(PipelineError::logic("lifecycle sink finish failed"));
            }
            Ok(())
        }
    }
    // cov:ignore-end

    #[derive(Default)]
    struct EofWritingFilter {
        events: Vec<(TokenType, Vec<u8>)>,
        eof_calls: usize,
    }

    impl TokenFilter for EofWritingFilter {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            self.events.push((token.token_type, token.raw.clone()));
            output.write_token(token)
        }

        fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            self.eof_calls += 1;
            output.write(b"!")
        }
    }

    struct FailOnWord(&'static str);

    impl TokenFilter for FailOnWord {
        fn handle_token(
            &mut self,
            token: &Token,
            _output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            if token.is_word_value(self.0.as_bytes()) {
                return Err(PipelineError::logic(format!("filter failed at {}", self.0)));
            }
            Ok(())
        }
    }

    struct FailOnEof;

    impl TokenFilter for FailOnEof {
        fn handle_token(
            &mut self,
            _token: &Token,
            _output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            Ok(())
        }

        fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            Err(PipelineError::logic("filter EOF failed"))
        }
    }

    #[derive(Default)]
    struct FailOnceOnWord {
        failed: bool,
    }

    impl TokenFilter for FailOnceOnWord {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            output.write_token(token)?;
            if token.is_word_value(b"q") && !self.failed {
                self.failed = true;
                return Err(PipelineError::logic("token callback failed once"));
            }
            Ok(())
        }

        fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            output.write(b"E")
        }
    }

    #[derive(Default)]
    struct FailOnceOnEof {
        failed: bool,
    }

    impl TokenFilter for FailOnceOnEof {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            output.write_token(token)
        }

        fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            if !self.failed {
                self.failed = true;
                output.write(b"F")?;
                return Err(PipelineError::logic("EOF callback failed once"));
            }
            output.write(b"E")
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RecordingRun {
        events: Vec<(TokenType, Vec<u8>)>,
        eof_calls: usize,
        output: Vec<u8>,
        downstream_finishes: usize,
    }

    fn run_recording(chunks: &[&[u8]], with_downstream: bool) -> PipelineResult<RecordingRun> {
        let mut filter = RecordingFilter::default();
        let mut sink = RecordingSink::default();
        {
            let next = with_downstream.then_some(&mut sink as &mut dyn Pipeline);
            let mut stage = QpdfTokenizer::new("token filter", &mut filter, next);
            for chunk in chunks {
                stage.write(chunk)?;
            }
            stage.finish()?;
        }
        Ok(RecordingRun {
            events: filter.events,
            eof_calls: filter.eof_calls,
            output: sink.chunks.concat(),
            downstream_finishes: sink.finishes,
        })
    }

    #[test]
    fn chunk_boundaries_do_not_change_tokens_or_output() {
        let input = b"%c\r\nBI /W 1 ID \0/F1 9 Tf EI /F2 12 Tf";
        let mut identifier_filter = RecordingFilter::default();
        let identifier_stage = QpdfTokenizer::new("token filter", &mut identifier_filter, None);
        assert_eq!(identifier_stage.identifier(), "token filter");
        let one = run_recording(&[input.as_slice()], true).unwrap();
        let bytewise_chunks = input.iter().map(std::slice::from_ref).collect::<Vec<_>>();
        let bytewise = run_recording(&bytewise_chunks, true).unwrap();
        assert_eq!(bytewise, one);
        assert_eq!(one.output, input);
        assert_eq!(one.eof_calls, 1);
        assert_eq!(one.downstream_finishes, 1);
        assert_eq!(
            one.events.iter().map(|(_, raw)| raw.len()).sum::<usize>(),
            input.len()
        );
    }

    #[test]
    fn shared_filter_preserves_downstream_pipeline_and_eof() {
        struct AppendOnEof;

        impl TokenFilter for AppendOnEof {
            fn handle_token(
                &mut self,
                token: &Token,
                output: &mut TokenFilterOutput<'_>,
            ) -> PipelineResult<()> {
                output.write_token(token)
            }

            fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
                output.write(b"E")
            }
        }

        let filter = Rc::new(RefCell::new(AppendOnEof));
        let mut sink = RecordingSink::default();
        let mut stage = QpdfTokenizer::new_shared(
            "shared token filter",
            filter.clone(),
            Some(PipelineRef::Borrowed(&mut sink)),
        );
        stage.write(b"q Q").unwrap();
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(sink.chunks.concat(), b"q QE");
        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn absent_downstream_discards_filter_output_but_delivers_all_callbacks() {
        let run = run_recording(&[b"/F1 12 Tf"], false).unwrap();
        assert!(run.output.is_empty());
        assert_eq!(run.eof_calls, 1);
        assert_eq!(run.events.last().unwrap().0, TokenType::Eof);
    }

    #[test]
    fn filter_failure_does_not_finish_downstream() {
        let mut sink = RecordingSink::default();
        let mut filter = FailOnWord("Tf");
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"/F1 12 Tf").unwrap();
        assert_eq!(stage.finish().unwrap_err().message(), "filter failed at Tf");
        drop(stage);
        assert_eq!(sink.finishes, 0);
    }

    #[test]
    fn downstream_finish_failure_is_returned_after_eof_callback() {
        let mut sink = FinishFailSink;
        let mut filter = RecordingFilter::default();
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        assert_eq!(stage.finish().unwrap_err().message(), "sink finish failed");
        drop(stage);
        assert_eq!(filter.eof_calls, 1);
    }

    #[test]
    fn dropping_without_finish_does_not_finish_downstream() {
        let mut sink = RecordingSink::default();
        let mut filter = RecordingFilter::default();
        {
            let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
            stage.write(b"q").unwrap();
        }
        assert_eq!(sink.finishes, 0);
        assert!(filter.events.is_empty());
    }

    #[test]
    fn handle_eof_failure_does_not_finish_downstream() {
        let mut sink = RecordingSink::default();
        let mut filter = FailOnEof;
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        assert_eq!(stage.finish().unwrap_err().message(), "filter EOF failed");
        drop(stage);
        assert_eq!(sink.finishes, 0);
    }

    #[test]
    fn second_finish_delivers_empty_eof_but_filter_output_stays_detached() {
        let mut sink = RecordingSink::default();
        let mut filter = EofWritingFilter::default();
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        stage.finish().unwrap();
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(filter.eof_calls, 2);
        assert_eq!(
            filter
                .events
                .iter()
                .filter(|(token_type, _)| *token_type == TokenType::Eof)
                .count(),
            2
        );
        assert_eq!(sink.finishes, 2);
        assert_eq!(sink.chunks.concat(), b"q!");
    }

    #[test]
    fn write_after_finish_delivers_callbacks_but_not_more_filter_output() {
        let mut sink = RecordingSink::default();
        let mut filter = EofWritingFilter::default();
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        stage.finish().unwrap();
        stage.write(b"Q").unwrap();
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(
            filter.events,
            vec![
                (TokenType::Word, b"q".to_vec()),
                (TokenType::Eof, Vec::new()),
                (TokenType::Word, b"Q".to_vec()),
                (TokenType::Eof, Vec::new()),
            ]
        );
        assert_eq!(sink.chunks.concat(), b"q!");
        assert_eq!(sink.finishes, 2);
    }

    #[test]
    fn downstream_finish_failure_retry_keeps_filter_output_detached() {
        let mut sink = FinishFailOnceSink::default();
        let mut filter = EofWritingFilter::default();
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        assert_eq!(
            stage.finish().unwrap_err().message(),
            "lifecycle sink finish failed"
        );
        stage.finish().unwrap();
        stage.write(b"Q").unwrap();
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(filter.eof_calls, 3);
        assert_eq!(
            filter.events,
            vec![
                (TokenType::Word, b"q".to_vec()),
                (TokenType::Eof, Vec::new()),
                (TokenType::Eof, Vec::new()),
                (TokenType::Word, b"Q".to_vec()),
                (TokenType::Eof, Vec::new()),
            ]
        );
        assert_eq!(sink.finishes, 3);
        assert_eq!(sink.chunks.concat(), b"q!");
    }

    #[test]
    fn callback_failure_consumes_the_failed_cycle_before_the_next_finish() {
        let mut sink = RecordingSink::default();
        let mut filter = FailOnWord("Tf");
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"Tf").unwrap();
        assert_eq!(stage.finish().unwrap_err().message(), "filter failed at Tf");
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(sink.finishes, 1);
    }

    #[test]
    fn token_callback_failure_keeps_filter_output_attached_for_retry() {
        let mut sink = RecordingSink::default();
        let mut filter = FailOnceOnWord::default();
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        assert_eq!(
            stage.finish().unwrap_err().message(),
            "token callback failed once"
        );
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(sink.finishes, 1);
        assert_eq!(sink.chunks.concat(), b"qE");
    }

    #[test]
    fn handle_eof_failure_keeps_filter_output_attached_for_retry() {
        let mut sink = RecordingSink::default();
        let mut filter = FailOnceOnEof::default();
        let mut stage = QpdfTokenizer::new("token filter", &mut filter, Some(&mut sink));
        stage.write(b"q").unwrap();
        assert_eq!(
            stage.finish().unwrap_err().message(),
            "EOF callback failed once"
        );
        stage.finish().unwrap();
        drop(stage);

        assert_eq!(sink.finishes, 1);
        assert_eq!(sink.chunks.concat(), b"qFE");
    }

    // cov:ignore-start: the ignored live qpdf oracle is separately run by qpdf-tokenizer-diff.sh
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

    fn hex_encode(data: &[u8]) -> String {
        data.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    struct ProbeRecordingFilter {
        records: String,
    }

    impl TokenFilter for ProbeRecordingFilter {
        fn handle_token(
            &mut self,
            token: &Token,
            output: &mut TokenFilterOutput<'_>,
        ) -> PipelineResult<()> {
            writeln!(
                self.records,
                "token\t{}\t{}",
                token_type_name(token.token_type),
                hex_encode(&token.raw),
            )
            .expect("writing a string cannot fail");
            output.write_token(token)
        }

        fn handle_eof(&mut self, _output: &mut TokenFilterOutput<'_>) -> PipelineResult<()> {
            writeln!(self.records, "eof-callback").expect("writing a string cannot fail");
            Ok(())
        }
    }

    fn dump_flpdf_token_filter(input: &[u8], chunks: &[usize]) -> String {
        let mut filter = ProbeRecordingFilter {
            records: String::new(),
        };
        let mut sink = RecordingSink::default();
        {
            let mut stage = QpdfTokenizer::new("token filter probe", &mut filter, Some(&mut sink));
            let mut offset = 0;
            for &chunk_len in chunks {
                let end = offset + chunk_len;
                stage.write(&input[offset..end]).unwrap();
                offset = end;
            }
            assert_eq!(offset, input.len());
            stage.finish().unwrap();
        }
        writeln!(
            filter.records,
            "output\t{}",
            hex_encode(&sink.chunks.concat())
        )
        .expect("writing a string cannot fail");
        filter.records
    }

    fn run_qpdf_token_filter_probe(probe: &Path, input: &[u8], chunks: &[usize]) -> String {
        let chunks = if chunks.len() == 1 && chunks[0] == input.len() {
            "all".to_string()
        } else {
            chunks
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        let output = Command::new(probe)
            .args([
                "--mode",
                "token-filter",
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
                &chunks,
            ])
            .output()
            .unwrap_or_else(|error| {
                panic!(
                    "failed to execute qpdf tokenizer probe {}: {error}",
                    probe.display()
                )
            });
        assert!(
            output.status.success(),
            "qpdf token-filter probe failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).expect("probe records are ASCII")
    }

    fn dump_flpdf_token_filter_lifecycle(input: &[u8]) -> String {
        let mut records = String::new();

        {
            let mut sink = RecordingSink::default();
            let mut filter = EofWritingFilter::default();
            let mut stage =
                QpdfTokenizer::new("reusable token filter", &mut filter, Some(&mut sink));
            stage.write(input).unwrap();
            stage.finish().unwrap();
            stage.finish().unwrap();
            stage.write(b"Q").unwrap();
            stage.finish().unwrap();
            drop(stage);
            writeln!(records, "reuse\ttokens\t{}", filter.events.len()).unwrap();
            writeln!(records, "reuse\teof-callbacks\t{}", filter.eof_calls).unwrap();
            writeln!(records, "reuse\tfinishes\t{}", sink.finishes).unwrap();
            writeln!(
                records,
                "reuse\toutput\t{}",
                hex_encode(&sink.chunks.concat())
            )
            .unwrap();
        }

        {
            let mut sink = FinishFailOnceSink::default();
            let mut filter = EofWritingFilter::default();
            let mut stage = QpdfTokenizer::new("retry token filter", &mut filter, Some(&mut sink));
            stage.write(input).unwrap();
            let error = stage.finish().unwrap_err();
            writeln!(records, "fail-retry\tfirst-error\t{}", error.message()).unwrap();
            stage.finish().unwrap();
            stage.write(b"Q").unwrap();
            stage.finish().unwrap();
            drop(stage);
            writeln!(records, "fail-retry\ttokens\t{}", filter.events.len()).unwrap();
            writeln!(records, "fail-retry\teof-callbacks\t{}", filter.eof_calls).unwrap();
            writeln!(records, "fail-retry\tfinishes\t{}", sink.finishes).unwrap();
            writeln!(
                records,
                "fail-retry\toutput\t{}",
                hex_encode(&sink.chunks.concat())
            )
            .unwrap();
        }

        records
    }

    fn run_qpdf_token_filter_lifecycle_probe(probe: &Path, input: &[u8]) -> String {
        let output = Command::new(probe)
            .args([
                "--mode",
                "token-filter-lifecycle",
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
            .unwrap_or_else(|error| {
                panic!(
                    "failed to execute qpdf token-filter lifecycle probe {}: {error}",
                    probe.display()
                )
            });
        assert!(
            output.status.success(),
            "qpdf token-filter lifecycle probe failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8(output.stdout).expect("probe records are ASCII")
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 token-filter oracle"]
    fn qpdf_token_filter_differential() {
        let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
            .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
        let cases = [
            ("empty", Vec::new(), vec![0]),
            (
                "comments-whitespace",
                b"% comment\x00\x09\x0a\x0c\x0d ".to_vec(),
                vec![15],
            ),
            (
                "escaped-names-strings",
                b"/A#20B (a\\(b\\)\\\\c)".to_vec(),
                vec![18],
            ),
            ("terminal-bad", b"(unterminated".to_vec(), vec![13]),
            ("terminal-id", b"BI /W 1 ID".to_vec(), vec![10]),
            (
                "inline-image-false-ei",
                b"BI /W 1 ID \x00not EI still-image\n EI Q".to_vec(),
                vec![36],
            ),
            ("bytewise", b"BI /W 1 ID \x00x EI Q".to_vec(), vec![1; 18]),
            (
                "split-around-id",
                b"BI /W 1 ID \x00x EI Q".to_vec(),
                vec![8, 2, 1, 7],
            ),
            (
                "split-around-ei",
                b"BI /W 1 ID \x00x EI Q".to_vec(),
                vec![14, 2, 2],
            ),
        ];
        for (name, input, chunks) in cases {
            assert_eq!(chunks.iter().sum::<usize>(), input.len(), "case {name}");
            assert_eq!(
                dump_flpdf_token_filter(&input, &chunks),
                run_qpdf_token_filter_probe(Path::new(&probe), &input, &chunks),
                "case {name}",
            );
        }
    }

    #[test]
    #[ignore = "live qpdf 11.9.0 token-filter lifecycle oracle"]
    fn qpdf_token_filter_lifecycle_differential() {
        let probe = std::env::var_os("QPDF_TOKENIZER_PROBE")
            .expect("set QPDF_TOKENIZER_PROBE to the built qpdf 11.9.0 probe");
        assert_eq!(
            dump_flpdf_token_filter_lifecycle(b"q"),
            run_qpdf_token_filter_lifecycle_probe(Path::new(&probe), b"q"),
        );
    }
    // cov:ignore-end
}
