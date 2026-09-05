//! qpdf correspondence: `QPDFAcroFormDocumentHelper.cc` `ResourceReplacer`.
//! Resource-name discovery uses the live `ObjectHandle` content callback route
//! (`QPDFObjectHandle.cc:1776-1847`, `ResourceFinder.cc:3-56`) before the
//! exact-byte token filter rewrites source names. A document-owned scan keeps
//! errors for the qpdf caller's catch-and-re-warn boundary; only the detached
//! in-memory route converts a structural failure to the byte-preserving
//! `Ok(None)` fallback.

use std::collections::BTreeMap;

use crate::content_stream::{
    parse_content_stream_handles, parse_content_stream_handles_with_recoverable_warnings,
};
use crate::object_handle::DocumentResolver;
use crate::pipeline::buffer::Buffer;
use crate::pipeline::qpdf_tokenizer::QpdfTokenizer;
use crate::pipeline::{Pipeline, PipelineError, PipelineResult};
use crate::resource_finder::{ResourceFinder, ResourceNamesByType};
use crate::token_filter::{TokenFilter, TokenFilterOutput};
use crate::tokenizer::{Token, TokenType};
use std::rc::Rc;

pub(crate) type ResourceRenames = BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, Vec<u8>>>;

pub(crate) struct ResourceReplacer {
    offset: usize,
    to_replace: BTreeMap<Vec<u8>, BTreeMap<usize, Vec<u8>>>,
}

fn name_value_from_decoded_body(body: &[u8]) -> Vec<u8> {
    let mut value = Vec::with_capacity(body.len() + 1);
    value.push(b'/');
    value.extend_from_slice(body);
    value
}

fn name_token_from_decoded_body(body: &[u8]) -> Token {
    Token::new(TokenType::Name, name_value_from_decoded_body(body))
}

impl ResourceReplacer {
    pub(crate) fn new(renames: &ResourceRenames, names: &ResourceNamesByType) -> Self {
        let mut to_replace = BTreeMap::new();

        for (resource_type, renamed_names) in renames {
            let Some(names_by_name) = names.get(resource_type) else {
                continue;
            };
            for (old_name, new_name) in renamed_names {
                let Some(offsets) = names_by_name.get(old_name) else {
                    continue;
                };
                let old_token_value = name_value_from_decoded_body(old_name);
                for offset in offsets {
                    to_replace
                        .entry(old_token_value.clone())
                        .or_insert_with(BTreeMap::new)
                        .insert(*offset, new_name.clone());
                }
            }
        }

        Self {
            offset: 0,
            to_replace,
        }
    }

    fn advance_offset(&mut self, length: usize) -> PipelineResult<()> {
        self.offset = self
            .offset
            .checked_add(length)
            .ok_or_else(|| PipelineError::runtime("ResourceReplacer offset overflow"))?;
        Ok(())
    }
}

impl TokenFilter for ResourceReplacer {
    fn handle_token(
        &mut self,
        token: &Token,
        output: &mut TokenFilterOutput<'_>,
    ) -> PipelineResult<()> {
        let replacement = (token.token_type == TokenType::Name)
            .then(|| self.to_replace.get(&token.value))
            .flatten()
            .and_then(|offsets| offsets.get(&self.offset));
        if let Some(new_name) = replacement {
            output.write_token(&name_token_from_decoded_body(new_name))?;
            self.advance_offset(token.raw.len())
        } else {
            self.advance_offset(token.raw.len())?;
            output.write_token(token)
        }
    }
}

#[cfg(test)]
pub(crate) fn replace_resource_names(
    input: &[u8],
    renames: &ResourceRenames,
) -> crate::Result<Option<Vec<u8>>> {
    replace_resource_names_with_context(input, renames, None)
}

pub(crate) fn replace_resource_names_with_context(
    input: &[u8],
    renames: &ResourceRenames,
    context: Option<Rc<dyn DocumentResolver>>,
) -> crate::Result<Option<Vec<u8>>> {
    if renames.is_empty() {
        return Ok(Some(input.to_vec()));
    }

    let has_document_context = context.is_some();
    let mut finder = ResourceFinder::default();
    let scan = match context {
        Some(context) => parse_content_stream_handles(input, Some(context), "", &mut finder),
        None => parse_content_stream_handles_with_recoverable_warnings(input, "", &mut finder),
    };
    if let Err(error) = scan {
        if has_document_context {
            // qpdf's document-owned parse catches this at the caller's
            // warning boundary. In particular, a warning sink failure must
            // not be converted into the ordinary malformed-content fallback.
            return Err(error);
        }
        // The detached/recoverable in-memory route has no qpdf object to
        // re-warn through, so structural failures retain the existing
        // byte-preserving fallback.
        return Ok(None);
    }

    let mut buffer = Buffer::new("ResourceReplacer buffer", None);
    let mut replacer = ResourceReplacer::new(renames, finder.names_by_resource_type());
    let mut tokenizer = QpdfTokenizer::new(
        "ResourceReplacer tokenizer",
        &mut replacer,
        Some(&mut buffer),
    );
    tokenizer.write(input)?;
    tokenizer.finish()?;
    drop(tokenizer);

    Ok(Some(buffer.take_buffer()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn font_renames(old_name: &[u8], new_name: &[u8]) -> ResourceRenames {
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Font".to_vec())
            .or_default()
            .insert(old_name.to_vec(), new_name.to_vec());
        renames
    }

    fn font_names(name: &[u8], offset: usize) -> ResourceNamesByType {
        let mut names = ResourceNamesByType::new();
        names
            .entry(b"Font".to_vec())
            .or_default()
            .insert(name.to_vec(), BTreeSet::from([offset]));
        names
    }

    #[derive(Default)]
    struct FailOnceWriteSink {
        failed: bool,
        bytes: Vec<u8>,
    }

    impl Pipeline for FailOnceWriteSink {
        // cov:ignore-start: mandatory test-sink metadata has no behavioral role
        fn identifier(&self) -> &str {
            "fail-once write sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            if !self.failed {
                self.failed = true;
                return Err(PipelineError::logic("sink write failed once"));
            }
            self.bytes.extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: mandatory test-sink finish is intentionally a no-op
        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    #[test]
    fn rewrites_only_name_and_offset_pairs_selected_by_finder() {
        let input = b"/F1 9 Tf /F1 10 Tj /F1 11 Tf";
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Font".to_vec())
            .or_default()
            .insert(b"F1".to_vec(), b"F A_1".to_vec());
        assert_eq!(
            replace_resource_names(input, &renames).unwrap().unwrap(),
            b"/F#20A_1 9 Tf /F1 10 Tj /F#20A_1 11 Tf"
        );
    }

    #[test]
    fn replacement_length_does_not_shift_source_offset_matching() {
        let input = b"/A 1 Tf /A 2 Tf";
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Font".to_vec())
            .or_default()
            .insert(b"A".to_vec(), b"MuchLonger".to_vec());
        assert_eq!(
            replace_resource_names(input, &renames).unwrap().unwrap(),
            b"/MuchLonger 1 Tf /MuchLonger 2 Tf"
        );
    }

    #[test]
    fn replacement_preserves_leading_slash_in_decoded_name_body() {
        let renames = font_renames(b"/F", b"/F_1");
        assert_eq!(
            replace_resource_names(b"/#2fF 12 Tf", &renames)
                .unwrap()
                .unwrap(),
            b"/#2fF_1 12 Tf"
        );
    }

    #[test]
    fn non_replacement_write_failure_advances_source_offset_before_retry() {
        let renames = font_renames(b"F", b"G");
        let names = font_names(b"F", 1);
        let mut replacer = ResourceReplacer::new(&renames, &names);
        let mut sink = FailOnceWriteSink::default();
        {
            let mut output = TokenFilterOutput::new(Some(&mut sink));

            assert_eq!(
                replacer
                    .handle_token(&Token::new(TokenType::Word, b"q".to_vec()), &mut output)
                    .unwrap_err()
                    .message(),
                "sink write failed once"
            );
            replacer
                .handle_token(&Token::new(TokenType::Name, b"/F".to_vec()), &mut output)
                .unwrap();
        }

        assert_eq!(sink.bytes, b"/G");
    }

    #[test]
    fn replacement_write_failure_keeps_source_offset_for_retry() {
        let renames = font_renames(b"F", b"G");
        let names = font_names(b"F", 0);
        let mut replacer = ResourceReplacer::new(&renames, &names);
        let mut sink = FailOnceWriteSink::default();
        let token = Token::new(TokenType::Name, b"/F".to_vec());
        {
            let mut output = TokenFilterOutput::new(Some(&mut sink));

            assert_eq!(
                replacer
                    .handle_token(&token, &mut output)
                    .unwrap_err()
                    .message(),
                "sink write failed once"
            );
            replacer.handle_token(&token, &mut output).unwrap();
        }

        assert_eq!(sink.bytes, b"/G");
    }

    #[test]
    fn rewrites_every_supported_resource_operator() {
        let cases = [
            (
                b"ColorSpace".as_slice(),
                b"CS1".as_slice(),
                b"/CS1 CS".as_slice(),
                b"/Renamed CS".as_slice(),
            ),
            (b"ColorSpace", b"CS1", b"/CS1 cs", b"/Renamed cs"),
            (b"ExtGState", b"GS1", b"/GS1 gs", b"/Renamed gs"),
            (b"Font", b"F1", b"/F1 12 Tf", b"/Renamed 12 Tf"),
            (b"Pattern", b"P1", b"/P1 SCN", b"/Renamed SCN"),
            (b"Pattern", b"P1", b"/P1 scn", b"/Renamed scn"),
            (b"Shading", b"Sh1", b"/Sh1 sh", b"/Renamed sh"),
            (b"XObject", b"X1", b"/X1 Do", b"/Renamed Do"),
        ];

        for (resource_type, old_name, input, expected) in cases {
            let mut renames = ResourceRenames::new();
            renames
                .entry(resource_type.to_vec())
                .or_default()
                .insert(old_name.to_vec(), b"Renamed".to_vec());
            assert_eq!(
                replace_resource_names(input, &renames).unwrap().unwrap(),
                expected,
            );
        }
    }

    #[test]
    fn rewrites_properties_name_for_bdc_and_dp() {
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Properties".to_vec())
            .or_default()
            .insert(b"P1".to_vec(), b"P1_1".to_vec());
        for (input, expected) in [
            (b"/Span /P1 BDC".as_slice(), b"/Span /P1_1 BDC".as_slice()),
            (b"/Span /P1 DP", b"/Span /P1_1 DP"),
        ] {
            assert_eq!(
                replace_resource_names(input, &renames).unwrap().unwrap(),
                expected,
            );
        }
    }

    #[test]
    fn inline_image_payload_and_unselected_tokens_are_byte_identical() {
        let input = b"%c\r\nBI ID /F1 8 Tf EI /F1 9 Tf";
        let renames = font_renames(b"F1", b"F2");
        assert_eq!(
            replace_resource_names(input, &renames).unwrap().unwrap(),
            b"%c\r\nBI ID /F1 8 Tf EI /F2 9 Tf"
        );
    }

    #[test]
    fn recoverable_finder_diagnostic_keeps_later_replacements() {
        let renames = font_renames(b"F1", b"F2");
        assert_eq!(
            replace_resource_names(b"<0g> /F1 9 Tf", &renames)
                .unwrap()
                .unwrap(),
            b"<0g> /F2 9 Tf"
        );
    }

    #[test]
    fn incomplete_inline_image_keeps_prefix_replacement_and_qpdf_separator() {
        let renames = font_renames(b"F1", b"F2");
        assert_eq!(
            replace_resource_names(b"/F1 12 Tf BI ID", &renames)
                .unwrap()
                .unwrap(),
            b"/F2 12 Tf BI ID "
        );
    }

    #[test]
    fn incomplete_inline_image_payload_keeps_prefix_replacement_only() {
        let renames = font_renames(b"F1", b"F2");
        assert_eq!(
            replace_resource_names(b"/F1 12 Tf BI ID /F1 8 Tf", &renames)
                .unwrap()
                .unwrap(),
            b"/F2 12 Tf BI ID /F1 8 Tf"
        );
    }

    #[test]
    fn fatal_structure_error_discards_collected_prefix_replacement() {
        let renames = font_renames(b"F1", b"F2");
        assert!(replace_resource_names(b"/F1 12 Tf [", &renames)
            .unwrap()
            .is_none());
    }
}
