#![expect(
    dead_code,
    reason = "Layer 1 syntax is intentionally pending production reader routing."
)]

use crate::parser::{is_ws, keyword_token_end, parse_qpdf_direct_object, Parser};
use crate::{Dictionary, Error, Object, ObjectRef, Result};

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
    InvalidStreamLength,
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
}
