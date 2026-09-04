//! JSON fatal-error paths must retain qpdf's raw source-name bytes.

use flpdf::{Error, Pdf};
use std::io::{self, Cursor, Read, Seek, SeekFrom};

fn import_error(json: &[u8]) -> Error {
    match Pdf::create_from_json(Cursor::new(json.to_vec()), b"json-input-\xff") {
        Ok(_) => panic!("malformed JSON input unexpectedly succeeded"),
        Err(error) => error,
    }
}

struct FailingPositionReader;

impl Read for FailingPositionReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl Seek for FailingPositionReader {
    fn seek(&mut self, _position: SeekFrom) -> io::Result<u64> {
        Err(io::Error::other("instrumented position failure"))
    }
}

#[test]
fn json_reactor_fatal_error_retains_raw_input_name() {
    let error = import_error(b"[]");
    assert_eq!(
        error.raw_message(),
        Some(b"json-input-\xff: QPDF JSON must be a dictionary".as_slice())
    );
}

#[test]
fn json_parser_error_retains_raw_input_name() {
    let error = import_error(b"{");
    assert_eq!(
        error.raw_message(),
        Some(b"json-input-\xff: JSON: premature end of input".as_slice())
    );
}

#[test]
fn json_parser_error_retains_raw_offending_byte() {
    let error = import_error(b"\xff");
    assert_eq!(
        error.raw_message(),
        Some(b"json-input-\xff: JSON: offset 0: unexpected character \xff".as_slice())
    );
}

#[test]
fn json_validation_error_retains_raw_input_name() {
    let error = import_error(b"{}");
    assert_eq!(
        error.raw_message(),
        Some(b"json-input-\xff: errors found in JSON".as_slice())
    );
}

#[test]
fn json_source_position_error_retains_raw_input_name() {
    let error = match Pdf::create_from_json(FailingPositionReader, b"json-input-\xff") {
        Ok(_) => panic!("position failure unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(
        error.raw_message(),
        Some(b"json-input-\xff: instrumented position failure".as_slice())
    );
}
