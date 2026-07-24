#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Layer 1 and 2 are intentionally pending production reader routing."
    )
)]

use crate::parser::{
    is_ws, keyword_token_end, parse_qpdf_direct_object, Parser, RecoveredStreamEol,
};
use crate::{Dictionary, Error, Object, ObjectRef, Result, Stream};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryPolicy {
    Strict,
    Bounded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamStartEol {
    Lf,
    CrLf,
    Cr,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileObjectDiagnosticKind {
    EmptyObject,
    StreamLineEnding,
    MissingStreamLength,
    InvalidStreamLength,
    NegativeStreamLength,
    ExpectedEndstream,
    AttemptingStreamLengthRecovery,
    RecoveredStreamLength { length: usize },
    EmptyRecoveredStream,
    ExpectedEndobj,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileObjectDiagnostic {
    pub(crate) kind: FileObjectDiagnosticKind,
    pub(crate) relative_offset: usize,
}

#[derive(Debug, PartialEq)]
pub(crate) enum PendingBody {
    Direct {
        object: Object,
        next_offset: usize,
    },
    Stream {
        dict: Dictionary,
        data_start: usize,
        start_eol: StreamStartEol,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) struct PendingFileObject {
    pub(crate) object_ref: ObjectRef,
    pub(crate) body: PendingBody,
    pub(crate) diagnostics: Vec<FileObjectDiagnostic>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct FileObjectRead {
    pub(crate) object_ref: ObjectRef,
    pub(crate) object: Object,
    pub(crate) diagnostics: Vec<FileObjectDiagnostic>,
    pub(crate) recovered_stream_eol: Option<RecoveredStreamEol>,
}

pub(crate) fn parse_file_object_syntax(input: &[u8]) -> Result<PendingFileObject> {
    let mut header = Parser::new(input);
    let number = header.integer_for_indirect()?;
    let generation = header.integer_for_indirect()?;
    header.expect_keyword_for_indirect(b"obj")?;
    header.skip_ws();
    let body_start = header.position();
    let parsed = parse_qpdf_direct_object(&input[body_start..])?;
    let object_ref = ObjectRef::new(
        u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
        u16::try_from(generation).map_err(|_| Error::parse(0, "invalid indirect generation"))?,
    );
    let next_offset = body_start + parsed.next_offset;
    let mut diagnostics = Vec::new();
    if let Some(empty_offset) = parsed.empty_offset {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::EmptyObject,
            relative_offset: body_start + empty_offset,
        });
    }

    if let Object::Dictionary(dict) = parsed.object {
        let stream_pos = skip_pdf_ws(input, next_offset);
        if let Some(after_stream) = keyword_token_end(input, stream_pos, b"stream") {
            let (data_start, start_eol) = consume_stream_start_eol(input, after_stream);
            if matches!(start_eol, StreamStartEol::Cr | StreamStartEol::Missing) {
                diagnostics.push(FileObjectDiagnostic {
                    kind: FileObjectDiagnosticKind::StreamLineEnding,
                    relative_offset: after_stream,
                });
            }
            return Ok(PendingFileObject {
                object_ref,
                body: PendingBody::Stream {
                    dict,
                    data_start,
                    start_eol,
                },
                diagnostics,
            });
        }
        return Ok(PendingFileObject {
            object_ref,
            body: PendingBody::Direct {
                object: Object::Dictionary(dict),
                next_offset,
            },
            diagnostics,
        });
    }

    Ok(PendingFileObject {
        object_ref,
        body: PendingBody::Direct {
            object: parsed.object,
            next_offset,
        },
        diagnostics,
    })
}

pub(crate) fn finish_file_object(
    input: &[u8],
    pending: PendingFileObject,
    resolved_indirect_length: Option<usize>,
    policy: RecoveryPolicy,
) -> Result<FileObjectRead> {
    let PendingFileObject {
        object_ref,
        body,
        mut diagnostics,
    } = pending;

    match body {
        PendingBody::Direct {
            object,
            next_offset,
        } => {
            check_endobj(input, next_offset, &mut diagnostics);
            Ok(FileObjectRead {
                object_ref,
                object,
                diagnostics,
                recovered_stream_eol: None,
            })
        }
        PendingBody::Stream {
            dict, data_start, ..
        } => finish_stream(
            input,
            object_ref,
            dict,
            data_start,
            resolved_indirect_length,
            policy,
            diagnostics,
        ),
    }
}

fn finish_stream(
    input: &[u8],
    object_ref: ObjectRef,
    dict: Dictionary,
    data_start: usize,
    resolved_indirect_length: Option<usize>,
    policy: RecoveryPolicy,
    mut diagnostics: Vec<FileObjectDiagnostic>,
) -> Result<FileObjectRead> {
    let (length, invalid_length) = match dict.get("Length") {
        Some(Object::Integer(value)) if *value < 0 => {
            diagnostics.push(FileObjectDiagnostic {
                kind: FileObjectDiagnosticKind::NegativeStreamLength,
                relative_offset: 0,
            });
            (Some(0), None)
        }
        Some(Object::Integer(value)) => (usize::try_from(*value).ok(), None),
        Some(Object::Reference(_)) => (
            resolved_indirect_length,
            resolved_indirect_length
                .is_none()
                .then_some(FileObjectDiagnosticKind::MissingStreamLength),
        ),
        None => (None, Some(FileObjectDiagnosticKind::MissingStreamLength)),
        Some(_) => (None, Some(FileObjectDiagnosticKind::InvalidStreamLength)),
    };
    let exact_end = length.and_then(|length| data_start.checked_add(length));
    let exact_terminator = exact_end.filter(|&end| end <= input.len()).and_then(|end| {
        let terminator = skip_pdf_ignorable(input, end);
        keyword_token_end(input, terminator, b"endstream").map(|after| (end, after))
    });

    let (data_end, after_endstream) = match exact_terminator {
        Some((end, after)) => (end, after),
        None if policy == RecoveryPolicy::Bounded => {
            if let Some(kind) = invalid_length.as_ref() {
                diagnostics.push(FileObjectDiagnostic {
                    kind: kind.clone(),
                    relative_offset: 0,
                });
            } else {
                diagnostics.push(FileObjectDiagnostic {
                    kind: FileObjectDiagnosticKind::ExpectedEndstream,
                    relative_offset: exact_end.unwrap_or(data_start),
                });
            }
            recover_stream_boundary(input, data_start, &mut diagnostics)
        }
        None => {
            let error_offset = if invalid_length.is_some() {
                0
            } else {
                exact_end.unwrap_or(data_start)
            };
            return Err(Error::parse(
                error_offset,
                invalid_length
                    .as_ref()
                    .map_or_else(|| "expected endstream".into(), |kind| kind.message()),
            ));
        }
    };

    check_endobj(input, after_endstream, &mut diagnostics);
    Ok(FileObjectRead {
        object_ref,
        object: Object::Stream(Stream::new(dict, input[data_start..data_end].to_vec())),
        diagnostics,
        recovered_stream_eol: None,
    })
}

fn check_endobj(input: &[u8], after_body: usize, diagnostics: &mut Vec<FileObjectDiagnostic>) {
    let expected = skip_pdf_ignorable(input, after_body);
    if keyword_token_end(input, expected, b"endobj").is_none() {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::ExpectedEndobj,
            relative_offset: expected,
        });
    }
}

fn recover_stream_boundary(
    input: &[u8],
    data_start: usize,
    diagnostics: &mut Vec<FileObjectDiagnostic>,
) -> (usize, usize) {
    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
        relative_offset: data_start,
    });

    if let Some(terminator) = find_recovery_terminator(input, data_start) {
        let data_end = terminator.position();
        let length = data_end - data_start;
        diagnostics.push(FileObjectDiagnostic {
            kind: if length == 0 {
                FileObjectDiagnosticKind::EmptyRecoveredStream
            } else {
                FileObjectDiagnosticKind::RecoveredStreamLength { length }
            },
            relative_offset: data_start,
        });
        return (data_end, terminator.after_body());
    }

    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::EmptyRecoveredStream,
        relative_offset: data_start,
    });
    (data_start, input.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryTerminator {
    Endstream { position: usize, after: usize },
    Endobj { position: usize },
}

impl RecoveryTerminator {
    const fn position(self) -> usize {
        match self {
            Self::Endstream { position, .. } | Self::Endobj { position } => position,
        }
    }

    const fn after_body(self) -> usize {
        match self {
            Self::Endstream { after, .. } => after,
            Self::Endobj { position } => position,
        }
    }
}

fn find_recovery_terminator(input: &[u8], start: usize) -> Option<RecoveryTerminator> {
    (start..input.len()).find_map(|position| {
        keyword_token_end(input, position, b"endstream")
            .map(|after| RecoveryTerminator::Endstream { position, after })
            .or_else(|| {
                keyword_token_end(input, position, b"endobj")
                    .map(|_| RecoveryTerminator::Endobj { position })
            })
    })
}

impl FileObjectDiagnosticKind {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::EmptyObject => "empty object treated as null".into(),
            Self::StreamLineEnding => {
                "stream keyword not followed by proper line terminator".into()
            }
            Self::MissingStreamLength => "stream dictionary lacks /Length key".into(),
            Self::InvalidStreamLength => {
                "/Length key in stream dictionary is not an integer".into()
            }
            Self::NegativeStreamLength => {
                "unsigned value request for negative number; returning 0".into()
            }
            Self::ExpectedEndstream => "expected endstream".into(),
            Self::AttemptingStreamLengthRecovery => "attempting to recover stream length".into(),
            Self::RecoveredStreamLength { length } => {
                format!("recovered stream length: {length}")
            }
            Self::EmptyRecoveredStream => {
                "unable to recover stream data; treating stream as empty".into()
            }
            Self::ExpectedEndobj => "expected endobj".into(),
        }
    }
}

impl PendingFileObject {
    pub(crate) fn indirect_length_ref(&self) -> Option<ObjectRef> {
        match &self.body {
            PendingBody::Stream { dict, .. } => dict.get_ref("Length"),
            PendingBody::Direct { .. } => None,
        }
    }
}

fn skip_pdf_ws(input: &[u8], mut pos: usize) -> usize {
    while input.get(pos).is_some_and(|&byte| is_ws(byte)) {
        pos += 1;
    }
    pos
}

fn skip_pdf_ignorable(input: &[u8], mut pos: usize) -> usize {
    loop {
        pos = skip_pdf_ws(input, pos);
        if input.get(pos) != Some(&b'%') {
            return pos;
        }
        while !matches!(input.get(pos), None | Some(b'\n' | b'\r')) {
            pos += 1;
        }
    }
}

fn consume_stream_start_eol(input: &[u8], pos: usize) -> (usize, StreamStartEol) {
    match input.get(pos..) {
        Some([b'\r', b'\n', ..]) => (pos + 2, StreamStartEol::CrLf),
        Some([b'\n', ..]) => (pos + 1, StreamStartEol::Lf),
        Some([b'\r', ..]) => (pos + 1, StreamStartEol::Cr),
        _ => (pos, StreamStartEol::Missing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_returns_pending_direct_and_empty_objects() {
        let direct = parse_file_object_syntax(b"4 0 obj\n[6 0 R]\nendobj\n").unwrap();
        assert_eq!(direct.object_ref, ObjectRef::new(4, 0));
        assert!(matches!(
            direct.body,
            PendingBody::Direct {
                object: Object::Array(_),
                ..
            }
        ));
        assert!(direct.diagnostics.is_empty());

        let empty = parse_file_object_syntax(b"5 0 obj\nendobj\n").unwrap();
        assert_eq!(
            empty.diagnostics,
            vec![FileObjectDiagnostic {
                kind: FileObjectDiagnosticKind::EmptyObject,
                relative_offset: 8,
            }]
        );
    }

    #[test]
    fn syntax_returns_pending_stream_without_reading_payload() {
        let input = b"7 0 obj\n<< /Length 9 0 R >>\nstream\nabcendstream\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        assert_eq!(pending.object_ref, ObjectRef::new(7, 0));
        assert_eq!(pending.indirect_length_ref(), Some(ObjectRef::new(9, 0)));
        let PendingBody::Stream {
            dict,
            data_start,
            start_eol,
        } = pending.body
        else {
            panic!("expected pending stream");
        };
        assert_eq!(dict.get_ref("Length"), Some(ObjectRef::new(9, 0)));
        assert_eq!(&input[data_start..data_start + 3], b"abc");
        assert_eq!(start_eol, StreamStartEol::Lf);
    }

    #[test]
    fn syntax_classifies_every_stream_start_line_ending() {
        for (suffix, expected, warns) in [
            (&b"\nabc"[..], StreamStartEol::Lf, false),
            (&b"\r\nabc"[..], StreamStartEol::CrLf, false),
            (&b"\rabc"[..], StreamStartEol::Cr, true),
            (&b" abc"[..], StreamStartEol::Missing, true),
        ] {
            let mut input = b"1 0 obj\n<< /Length 3 >>\nstream".to_vec();
            input.extend_from_slice(suffix);
            input.extend_from_slice(b"endstream\nendobj\n");
            let pending = parse_file_object_syntax(&input).unwrap();
            assert!(matches!(
                pending.body,
                PendingBody::Stream { start_eol, .. } if start_eol == expected
            ));
            assert_eq!(
                pending
                    .diagnostics
                    .iter()
                    .any(|d| d.kind == FileObjectDiagnosticKind::StreamLineEnding),
                warns
            );
        }
    }

    #[test]
    fn exact_lengths_accept_eol_and_adjacent_endstream_payloads() {
        for (payload, tail) in [
            (&b"abc"[..], &b"endstream\nendobj\n"[..]),
            (&b"abc\n"[..], &b"endstream\nendobj\n"[..]),
            (&b"abc\r\n"[..], &b"endstream\nendobj\n"[..]),
        ] {
            let mut input = b"7 0 obj\n<< /Length 9 0 R >>\nstream\n".to_vec();
            input.extend_from_slice(payload);
            input.extend_from_slice(tail);
            let pending = parse_file_object_syntax(&input).unwrap();
            let completed = finish_file_object(
                &input,
                pending,
                Some(payload.len()),
                RecoveryPolicy::Bounded,
            )
            .unwrap();
            assert_eq!(completed.object.as_stream().unwrap().data, payload);
            assert!(!completed
                .diagnostics
                .iter()
                .any(|d| d.kind == FileObjectDiagnosticKind::ExpectedEndobj));
        }
    }

    #[test]
    fn endstream_and_endobj_are_separate_results() {
        let input = b"1 0 obj\n<< /Length 3 >>\nstream\nabcendstream\nnot-endobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        let completed = finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(completed.object.as_stream().unwrap().data, b"abc");
        assert_eq!(
            completed.diagnostics.last().unwrap().kind,
            FileObjectDiagnosticKind::ExpectedEndobj
        );
    }

    #[test]
    fn exact_boundary_rejects_endstream_substring_without_token_end() {
        let input = b"1 0 obj\n<< /Length 3 >>\nstream\nabcendstreamX\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        assert!(finish_file_object(input, pending, None, RecoveryPolicy::Strict).is_err());
    }

    #[test]
    fn exact_length_allows_tokenizer_ignorable_bytes_before_terminators() {
        for endstream_prefix in [&b"\n"[..], &b"% framing comment\r\n"[..]] {
            let mut input = b"1 0 obj\n<< /Length 3 >>\nstream\nabc".to_vec();
            input.extend_from_slice(endstream_prefix);
            input.extend_from_slice(b"endstream% separator\nendobj\n");
            let pending = parse_file_object_syntax(&input).unwrap();
            let completed =
                finish_file_object(&input, pending, None, RecoveryPolicy::Strict).unwrap();
            assert_eq!(completed.object.as_stream().unwrap().data, b"abc");
            assert!(completed.diagnostics.is_empty());
        }
    }

    #[test]
    fn direct_completion_checks_endobj_without_rejecting_the_object() {
        let complete = b"4 0 obj\n[6 0 R]\nendobj\n";
        let completed = finish_file_object(
            complete,
            parse_file_object_syntax(complete).unwrap(),
            None,
            RecoveryPolicy::Strict,
        )
        .unwrap();
        assert!(matches!(completed.object, Object::Array(_)));
        assert!(completed.diagnostics.is_empty());
        assert_eq!(completed.recovered_stream_eol, None);

        let missing = b"4 0 obj\n[6 0 R]\nnot-endobj\n";
        let completed = finish_file_object(
            missing,
            parse_file_object_syntax(missing).unwrap(),
            None,
            RecoveryPolicy::Strict,
        )
        .unwrap();
        assert_eq!(
            completed.diagnostics.last().unwrap().kind,
            FileObjectDiagnosticKind::ExpectedEndobj
        );
    }

    #[test]
    fn recovery_matrix_is_bounded_token_aware_and_ordered() {
        struct Case {
            length: &'static [u8],
            payload: &'static [u8],
            terminator: &'static [u8],
            recovered: &'static [u8],
            kinds: Vec<FileObjectDiagnosticKind>,
        }
        let cases = [
            Case {
                length: b"/Length /Bad",
                payload: b"abc\n",
                terminator: b"endstream\nendobj\n",
                recovered: b"abc\n",
                kinds: vec![
                    FileObjectDiagnosticKind::InvalidStreamLength,
                    FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                    FileObjectDiagnosticKind::RecoveredStreamLength { length: 4 },
                ],
            },
            Case {
                length: b"/Length -1",
                payload: b"\n",
                terminator: b"endstream\nendobj\n",
                recovered: b"",
                kinds: vec![FileObjectDiagnosticKind::NegativeStreamLength],
            },
            Case {
                length: b"/Length 2",
                payload: b"abc\r\n",
                terminator: b"endstream\nendobj\n",
                recovered: b"abc\r\n",
                kinds: vec![
                    FileObjectDiagnosticKind::ExpectedEndstream,
                    FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                    FileObjectDiagnosticKind::RecoveredStreamLength { length: 5 },
                ],
            },
            Case {
                length: b"/Length 99",
                payload: b"abc\n",
                terminator: b"endstream\nendobj\n",
                recovered: b"abc\n",
                kinds: vec![
                    FileObjectDiagnosticKind::ExpectedEndstream,
                    FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                    FileObjectDiagnosticKind::RecoveredStreamLength { length: 4 },
                ],
            },
        ];

        for case in cases {
            let mut input = b"1 0 obj\n<< ".to_vec();
            input.extend_from_slice(case.length);
            input.extend_from_slice(b" >>\nstream\n");
            input.extend_from_slice(case.payload);
            input.extend_from_slice(case.terminator);
            let pending = parse_file_object_syntax(&input).unwrap();
            let completed =
                finish_file_object(&input, pending, None, RecoveryPolicy::Bounded).unwrap();
            assert_eq!(completed.object.as_stream().unwrap().data, case.recovered);
            assert_eq!(
                completed
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.kind)
                    .collect::<Vec<_>>(),
                case.kinds
            );
            assert_eq!(completed.recovered_stream_eol, None);
        }
    }

    #[test]
    fn missing_and_unresolved_lengths_use_the_missing_length_diagnostic() {
        for dictionary in [&b"<< >>"[..], &b"<< /Length 9 0 R >>"[..]] {
            let mut input = b"1 0 obj\n".to_vec();
            input.extend_from_slice(dictionary);
            input.extend_from_slice(b"\nstream\nabc\nendstream\nendobj\n");
            let pending = parse_file_object_syntax(&input).unwrap();
            let completed =
                finish_file_object(&input, pending, None, RecoveryPolicy::Bounded).unwrap();
            assert_eq!(completed.object_ref, ObjectRef::new(1, 0));
            assert_eq!(completed.object.as_stream().unwrap().data, b"abc\n");
            assert_eq!(
                completed
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.kind)
                    .collect::<Vec<_>>(),
                vec![
                    FileObjectDiagnosticKind::MissingStreamLength,
                    FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                    FileObjectDiagnosticKind::RecoveredStreamLength { length: 4 },
                ]
            );
        }
    }

    #[test]
    fn strict_invalid_length_returns_the_qpdf_diagnostic_text() {
        let input = b"1 0 obj\n<< /Length /Bad >>\nstream\nabc\nendstream\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        let error = finish_file_object(input, pending, None, RecoveryPolicy::Strict).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::Parse { offset: 0, message }
                    if message == "/Length key in stream dictionary is not an integer"
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn recovery_ignores_keyword_substrings_inside_payload() {
        let input = b"1 0 obj\n<< /Length /Bad >>\nstream\n\
                      AendstreamXB endobjY C\nendstream\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        let completed = finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(
            completed.object.as_stream().unwrap().data,
            b"AendstreamXB endobjY C\n"
        );
    }

    #[test]
    fn missing_endstream_recovers_at_endobj_without_an_extra_expected_warning() {
        let input = b"1 0 obj\n<< /Length /Bad >>\nstream\nabc\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        let completed = finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(completed.object.as_stream().unwrap().data, b"abc\n");
        assert_eq!(
            completed
                .diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.kind)
                .collect::<Vec<_>>(),
            vec![
                FileObjectDiagnosticKind::InvalidStreamLength,
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                FileObjectDiagnosticKind::RecoveredStreamLength { length: 4 },
            ]
        );
    }

    #[test]
    fn recovery_uses_the_first_end_token_and_qpdfs_right_boundary_rule() {
        let input = b"1 0 obj\n<< /Length /Bad >>\nstream\nAendstream\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        let completed = finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(completed.object.as_stream().unwrap().data, b"A");

        let input = b"1 0 obj\n<< /Length /Bad >>\nstream\nabc\nendobj\njunk\nendstream\nendobj\n";
        let pending = parse_file_object_syntax(input).unwrap();
        let completed = finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
        assert_eq!(completed.object.as_stream().unwrap().data, b"abc\n");
        assert!(!completed
            .diagnostics
            .iter()
            .any(|d| d.kind == FileObjectDiagnosticKind::ExpectedEndobj));
    }

    #[test]
    fn unrecoverable_and_zero_length_recovery_return_qpdf_empty_streams() {
        for (input, expects_endobj) in [
            (&b"1 0 obj\n<< /Length /Bad >>\nstream\ntruncated"[..], true),
            (
                &b"1 0 obj\n<< /Length /Bad >>\nstream\nendstream\nendobj\n"[..],
                false,
            ),
        ] {
            let pending = parse_file_object_syntax(input).unwrap();
            let completed =
                finish_file_object(input, pending, None, RecoveryPolicy::Bounded).unwrap();
            assert!(completed.object.as_stream().unwrap().data.is_empty());
            assert_eq!(
                completed
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.kind.clone())
                    .collect::<Vec<_>>(),
                if expects_endobj {
                    vec![
                        FileObjectDiagnosticKind::InvalidStreamLength,
                        FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                        FileObjectDiagnosticKind::EmptyRecoveredStream,
                        FileObjectDiagnosticKind::ExpectedEndobj,
                    ]
                } else {
                    vec![
                        FileObjectDiagnosticKind::InvalidStreamLength,
                        FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                        FileObjectDiagnosticKind::EmptyRecoveredStream,
                    ]
                }
            );
        }
    }

    #[test]
    fn diagnostic_messages_match_qpdf_11_9_0() {
        for (kind, message) in [
            (
                FileObjectDiagnosticKind::EmptyObject,
                "empty object treated as null".to_string(),
            ),
            (
                FileObjectDiagnosticKind::StreamLineEnding,
                "stream keyword not followed by proper line terminator".to_string(),
            ),
            (
                FileObjectDiagnosticKind::MissingStreamLength,
                "stream dictionary lacks /Length key".to_string(),
            ),
            (
                FileObjectDiagnosticKind::InvalidStreamLength,
                "/Length key in stream dictionary is not an integer".to_string(),
            ),
            (
                FileObjectDiagnosticKind::NegativeStreamLength,
                "unsigned value request for negative number; returning 0".to_string(),
            ),
            (
                FileObjectDiagnosticKind::ExpectedEndstream,
                "expected endstream".to_string(),
            ),
            (
                FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
                "attempting to recover stream length".to_string(),
            ),
            (
                FileObjectDiagnosticKind::RecoveredStreamLength { length: 4 },
                "recovered stream length: 4".to_string(),
            ),
            (
                FileObjectDiagnosticKind::EmptyRecoveredStream,
                "unable to recover stream data; treating stream as empty".to_string(),
            ),
            (
                FileObjectDiagnosticKind::ExpectedEndobj,
                "expected endobj".to_string(),
            ),
        ] {
            assert_eq!(kind.message(), message);
        }
    }
}
