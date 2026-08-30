//! qpdf correspondence: QPDF.cc readObject/readStream framing and recovery split from the document reader.
use crate::parser::{
    keyword_token_end, parse_qpdf_file_object_handle_with_diagnostics, HandleResolver,
    RecoveredStreamEol,
};
use crate::tokenizer::{is_ws, Tokenizer};
use crate::{Error, ObjectHandle, ObjectRef, Result};
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryPolicy {
    Strict,
    Bounded,
    RequireEndstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedStreamLength {
    Missing,
    Invalid,
    Integer(i64),
}

/// A framing EOL observed immediately before a recovered terminator that is
/// still included in the completed handle's raw stream data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncludedStreamDataEol {
    Lf,
    Cr,
    CrLf,
}

impl IncludedStreamDataEol {
    const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::Cr => b"\r",
            Self::CrLf => b"\r\n",
        }
    }

    const fn as_removed(self) -> RecoveredStreamEol {
        match self {
            Self::Lf => RecoveredStreamEol::Lf,
            Self::Cr => RecoveredStreamEol::Cr,
            Self::CrLf => RecoveredStreamEol::CrLf,
        }
    }
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
    StreamCarriageReturnOnly,
    StreamMissingLineTerminator,
    MissingStreamLength,
    InvalidStreamLength,
    NegativeStreamLength,
    ExpectedEndstream,
    AttemptingStreamLengthRecovery,
    RecoveredStreamLength { length: usize },
    EmptyRecoveredStream,
    ExpectedEndobj,
    TokenizerWarning { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileObjectDiagnostic {
    pub(crate) kind: FileObjectDiagnosticKind,
    pub(crate) relative_offset: usize,
}

/// Handle-native counterpart of qpdf's `readObjectAtOffset`/`readStream`
/// framing (`libqpdf/QPDF.cc:1298-1395,1591-1623`). The dictionary and every
/// indirect child remain handles while only the byte payload is copied into
/// the stream buffer required by the filter pipeline.
#[derive(Debug)]
pub(crate) enum PendingHandleBody {
    Direct {
        object: ObjectHandle,
        next_offset: usize,
    },
    Stream {
        dict: ObjectHandle,
        data_start: usize,
    },
}

#[derive(Debug)]
pub(crate) struct PendingHandleFileObject {
    pub(crate) object_ref: ObjectRef,
    pub(crate) body: PendingHandleBody,
    pub(crate) diagnostics: Vec<FileObjectDiagnostic>,
}

#[derive(Debug)]
pub(crate) struct HandleFileObjectRead {
    pub(crate) object_ref: ObjectRef,
    pub(crate) object: ObjectHandle,
    pub(crate) diagnostics: Vec<FileObjectDiagnostic>,
    pub(crate) included_recovery_eol: Option<IncludedStreamDataEol>,
}

fn remove_included_recovery_eol_from_handle(
    object: &ObjectHandle,
    included_recovery_eol: &mut Option<IncludedStreamDataEol>,
) -> Option<RecoveredStreamEol> {
    let included = (*included_recovery_eol)?;
    let data = object
        .as_stream_data()
        .expect("included recovery EOL belongs to a stream");
    let eol = included.as_bytes();
    assert!(
        data.ends_with(eol),
        "included recovery EOL must remain in raw stream data"
    );
    let mut data = (*data).clone();
    data.truncate(data.len() - eol.len());
    object.replace_stream_data(Rc::new(data), None, None);
    *included_recovery_eol = None;
    Some(included.as_removed())
}

impl HandleFileObjectRead {
    pub(crate) fn remove_included_recovery_eol_for_decryption(
        &mut self,
    ) -> Option<RecoveredStreamEol> {
        remove_included_recovery_eol_from_handle(&self.object, &mut self.included_recovery_eol)
    }
}

pub(crate) fn parse_file_object_handle_syntax(
    input: &[u8],
    resolver: &mut dyn HandleResolver,
) -> Result<PendingHandleFileObject> {
    let mut tokenizer = Tokenizer::new(input);
    let object_ref = parse_file_object_header_tokens(&mut tokenizer)?;
    tokenizer.skip_ignorable()?;
    let body_start = tokenizer.position();
    let parsed = parse_qpdf_file_object_handle_with_diagnostics(
        &input[body_start..],
        i64::try_from(body_start).unwrap_or(i64::MAX),
        Some(i64::try_from(body_start).unwrap_or(i64::MAX)),
        resolver,
    )
    .map_err(|error| error.rebase_offset(body_start))?;
    let next_offset = body_start.saturating_add(parsed.next_offset);
    let mut diagnostics = parsed
        .diagnostics
        .into_iter()
        .map(|diagnostic| FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::TokenizerWarning {
                message: diagnostic.message,
            },
            relative_offset: body_start.saturating_add(diagnostic.relative_offset),
        })
        .collect::<Vec<_>>();
    if let Some(empty_offset) = parsed.empty_offset {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::EmptyObject,
            relative_offset: body_start.saturating_add(empty_offset),
        });
    }

    let object = parsed.value;
    if object.as_dictionary().is_some() {
        let stream_pos = skip_pdf_ws(input, next_offset);
        if let Some(after_stream) = keyword_token_end(input, stream_pos, b"stream") {
            let (data_start, start_eol) = consume_stream_start_eol(input, after_stream);
            if let Some(kind) = match start_eol {
                StreamStartEol::Cr => Some(FileObjectDiagnosticKind::StreamCarriageReturnOnly),
                StreamStartEol::Missing => {
                    Some(FileObjectDiagnosticKind::StreamMissingLineTerminator)
                }
                StreamStartEol::Lf | StreamStartEol::CrLf => None,
            } {
                diagnostics.push(FileObjectDiagnostic {
                    kind,
                    relative_offset: after_stream,
                });
            }
            return Ok(PendingHandleFileObject {
                object_ref,
                body: PendingHandleBody::Stream {
                    dict: object,
                    data_start,
                },
                diagnostics,
            });
        }
    }

    Ok(PendingHandleFileObject {
        object_ref,
        body: PendingHandleBody::Direct {
            object,
            next_offset,
        },
        diagnostics,
    })
}

pub(crate) fn parse_file_object_header(input: &[u8]) -> Result<ObjectRef> {
    let mut tokenizer = Tokenizer::new(input);
    parse_file_object_header_tokens(&mut tokenizer)
}

fn parse_file_object_header_tokens(tokenizer: &mut Tokenizer<'_>) -> Result<ObjectRef> {
    let number = tokenizer.next_integer()?;
    let generation = tokenizer.next_integer()?;
    tokenizer.expect_word(b"obj")?;
    Ok(ObjectRef::new(
        u32::try_from(number).map_err(|_| Error::parse(0, "invalid indirect object number"))?,
        u16::try_from(generation).map_err(|_| Error::parse(0, "invalid indirect generation"))?,
    ))
}

pub(crate) fn finish_file_object_handle(
    input: &[u8],
    pending: PendingHandleFileObject,
    resolved_indirect_length: Option<ResolvedStreamLength>,
    policy: RecoveryPolicy,
) -> Result<HandleFileObjectRead> {
    let PendingHandleFileObject {
        object_ref,
        body,
        mut diagnostics,
    } = pending;

    match body {
        PendingHandleBody::Direct {
            object,
            next_offset,
        } => {
            check_endobj(input, next_offset, &mut diagnostics)?;
            Ok(HandleFileObjectRead {
                object_ref,
                object,
                diagnostics,
                included_recovery_eol: None,
            })
        }
        PendingHandleBody::Stream {
            dict, data_start, ..
        } => finish_handle_stream(
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

fn finish_handle_stream(
    input: &[u8],
    object_ref: ObjectRef,
    dict: ObjectHandle,
    data_start: usize,
    resolved_indirect_length: Option<ResolvedStreamLength>,
    policy: RecoveryPolicy,
    diagnostics: Vec<FileObjectDiagnostic>,
) -> Result<HandleFileObjectRead> {
    let mut completed = complete_handle_stream(
        input,
        dict,
        data_start,
        resolved_indirect_length,
        policy,
        diagnostics,
    )?;
    check_endobj(input, completed.after_endstream, &mut completed.diagnostics)?;
    Ok(HandleFileObjectRead {
        object_ref,
        object: completed.object,
        diagnostics: completed.diagnostics,
        included_recovery_eol: completed.included_recovery_eol,
    })
}

struct CompletedHandleStream {
    object: ObjectHandle,
    diagnostics: Vec<FileObjectDiagnostic>,
    included_recovery_eol: Option<IncludedStreamDataEol>,
    after_endstream: usize,
}

fn complete_handle_stream(
    input: &[u8],
    dict: ObjectHandle,
    data_start: usize,
    resolved_indirect_length: Option<ResolvedStreamLength>,
    policy: RecoveryPolicy,
    mut diagnostics: Vec<FileObjectDiagnostic>,
) -> Result<CompletedHandleStream> {
    let length_entry = dict
        .as_dictionary()
        .and_then(|entries| entries.get(b"/Length".as_slice()).cloned());
    let resolved_length = match length_entry {
        Some(entry) if entry.object_ref().is_some() => match resolved_indirect_length {
            Some(value) => value,
            None => match entry.try_as_integer()? {
                Some(value) => ResolvedStreamLength::Integer(value),
                None => ResolvedStreamLength::Missing,
            },
        },
        Some(entry) => match entry.try_as_integer()? {
            Some(value) => ResolvedStreamLength::Integer(value),
            None => ResolvedStreamLength::Invalid,
        },
        None => ResolvedStreamLength::Missing,
    };
    let (length, invalid_length) = match resolved_length {
        ResolvedStreamLength::Integer(value) if value < 0 => {
            diagnostics.push(FileObjectDiagnostic {
                kind: FileObjectDiagnosticKind::NegativeStreamLength,
                relative_offset: 0,
            });
            (Some(0), None)
        }
        ResolvedStreamLength::Integer(value) => {
            let length = usize::try_from(value).ok();
            let invalid = length
                .is_none()
                .then_some(FileObjectDiagnosticKind::InvalidStreamLength);
            (length, invalid)
        }
        ResolvedStreamLength::Missing => {
            (None, Some(FileObjectDiagnosticKind::MissingStreamLength))
        }
        ResolvedStreamLength::Invalid => {
            (None, Some(FileObjectDiagnosticKind::InvalidStreamLength))
        }
    };
    let exact_end = length.and_then(|length| data_start.checked_add(length));
    let usable_length = matches!(
        resolved_length,
        ResolvedStreamLength::Integer(value) if value >= 0
    ) && exact_end.is_some_and(|end| end <= input.len());
    let exact_terminator = if let Some(end) = exact_end.filter(|&end| end <= input.len()) {
        let terminator = skip_pdf_ignorable(input, end)?;
        keyword_token_end(input, terminator, b"endstream").map(|after| (end, after))
    } else {
        None
    };

    let (data_end, after_endstream, included_recovery_eol) = match exact_terminator {
        Some((end, after)) => (end, after, None),
        None if policy == RecoveryPolicy::RequireEndstream && usable_length => {
            return Err(Error::parse(
                exact_end.expect("usable stream length has an exact boundary"),
                "expected endstream",
            ));
        }
        None if policy != RecoveryPolicy::Strict => {
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
            match recover_stream_boundary(input, data_start, policy, &mut diagnostics) {
                Some((end, after)) => {
                    (end, after, included_stream_data_eol(input, data_start, end))
                }
                None if policy == RecoveryPolicy::RequireEndstream => {
                    return Err(Error::parse(data_start, "stream data exceeds input"));
                }
                None => (data_start, input.len(), None),
            }
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

    Ok(CompletedHandleStream {
        object: ObjectHandle::stream(dict, Rc::new(input[data_start..data_end].to_vec())),
        diagnostics,
        included_recovery_eol,
        after_endstream,
    })
}

fn check_endobj(
    input: &[u8],
    after_body: usize,
    diagnostics: &mut Vec<FileObjectDiagnostic>,
) -> Result<()> {
    let expected = skip_pdf_ignorable(input, after_body)?;
    if keyword_token_end(input, expected, b"endobj").is_none() {
        diagnostics.push(FileObjectDiagnostic {
            kind: FileObjectDiagnosticKind::ExpectedEndobj,
            relative_offset: expected,
        });
    }
    Ok(())
}

fn recover_stream_boundary(
    input: &[u8],
    data_start: usize,
    policy: RecoveryPolicy,
    diagnostics: &mut Vec<FileObjectDiagnostic>,
) -> Option<(usize, usize)> {
    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::AttemptingStreamLengthRecovery,
        relative_offset: data_start,
    });

    let terminator = match policy {
        RecoveryPolicy::RequireEndstream => {
            find_line_anchored_endstream_terminator(input, data_start)
        }
        RecoveryPolicy::Strict | RecoveryPolicy::Bounded => {
            find_recovery_terminator(input, data_start)
        }
    };
    if let Some(terminator) = terminator {
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
        return Some((data_end, terminator.after_body()));
    }

    diagnostics.push(FileObjectDiagnostic {
        kind: FileObjectDiagnosticKind::EmptyRecoveredStream,
        relative_offset: data_start,
    });
    None
}

#[cfg(test)]
mod final_handle_tests {
    use super::{finish_file_object_handle, parse_file_object_handle_syntax, RecoveryPolicy};
    use crate::object_handle::{ObjectHandle, ObjectValue};
    use crate::parser::HandleResolver;
    use crate::{ObjectRef, Result};

    struct Detached;

    impl HandleResolver for Detached {
        fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
            ObjectHandle::new_indirect_unresolved(object_ref, -1)
        }

        fn direct_handle(&mut self, value: ObjectValue) -> ObjectHandle {
            ObjectHandle::from_value(value)
        }
    }

    #[test]
    fn require_endstream_rejects_a_usable_length_without_a_terminator() -> Result<()> {
        let input = b"1 0 obj\n<< /Length 3 >>\nstream\nabc\nnot-endobj";
        let mut resolver = Detached;
        let pending = parse_file_object_handle_syntax(input, &mut resolver)?;
        let error =
            finish_file_object_handle(input, pending, None, RecoveryPolicy::RequireEndstream)
                .expect_err("strict qpdf stream framing requires endstream");
        assert!(error.to_string().contains("expected endstream"));
        Ok(())
    }

    #[test]
    fn require_endstream_rejects_unbounded_stream_data_without_a_terminator() -> Result<()> {
        let input = b"1 0 obj\n<< >>\nstream\nabc";
        let mut resolver = Detached;
        let pending = parse_file_object_handle_syntax(input, &mut resolver)?;
        let error =
            finish_file_object_handle(input, pending, None, RecoveryPolicy::RequireEndstream)
                .expect_err("strict qpdf stream framing requires a recoverable endstream");
        assert!(error.to_string().contains("stream data exceeds input"));
        Ok(())
    }
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

fn find_line_anchored_endstream_terminator(
    input: &[u8],
    start: usize,
) -> Option<RecoveryTerminator> {
    (start..input.len())
        .filter(|&position| {
            position == start || matches!(input.get(position - 1), Some(b'\n' | b'\r'))
        })
        .find_map(|position| {
            keyword_token_end(input, position, b"endstream")
                .map(|after| RecoveryTerminator::Endstream { position, after })
        })
}

fn included_stream_data_eol(
    input: &[u8],
    data_start: usize,
    data_end: usize,
) -> Option<IncludedStreamDataEol> {
    if data_end >= data_start + 2 && input.get(data_end - 2..data_end) == Some(&b"\r\n"[..]) {
        Some(IncludedStreamDataEol::CrLf)
    } else if data_end > data_start && input.get(data_end - 1) == Some(&b'\n') {
        Some(IncludedStreamDataEol::Lf)
    } else if data_end > data_start && input.get(data_end - 1) == Some(&b'\r') {
        Some(IncludedStreamDataEol::Cr)
    } else {
        None
    }
}

impl FileObjectDiagnosticKind {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::EmptyObject => "empty object treated as null".into(),
            Self::StreamCarriageReturnOnly => {
                "stream keyword followed by carriage return only".into()
            }
            Self::StreamMissingLineTerminator => {
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
            Self::TokenizerWarning { message } => message.clone(),
        }
    }
}

impl PendingHandleFileObject {
    pub(crate) fn indirect_length_ref(&self) -> Option<ObjectRef> {
        match &self.body {
            PendingHandleBody::Stream { dict, .. } => dict
                .as_dictionary()?
                .get(b"/Length".as_slice())
                .and_then(ObjectHandle::object_ref),
            PendingHandleBody::Direct { .. } => None,
        }
    }
}

fn skip_pdf_ws(input: &[u8], mut pos: usize) -> usize {
    while input.get(pos).is_some_and(|&byte| is_ws(byte)) {
        pos += 1;
    }
    pos
}

fn skip_pdf_ignorable(input: &[u8], pos: usize) -> Result<usize> {
    let mut tokenizer = Tokenizer::new(input);
    tokenizer.set_position(pos)?;
    tokenizer.skip_ignorable()?;
    Ok(tokenizer.position())
}

fn consume_stream_start_eol(input: &[u8], pos: usize) -> (usize, StreamStartEol) {
    match input.get(pos..) {
        Some([b'\r', b'\n', ..]) => (pos + 2, StreamStartEol::CrLf),
        Some([b'\n', ..]) => (pos + 1, StreamStartEol::Lf),
        Some([b'\r', ..]) => (pos + 1, StreamStartEol::Cr),
        _ => (pos, StreamStartEol::Missing),
    }
}
