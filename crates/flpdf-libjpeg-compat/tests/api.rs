#![cfg(feature = "system-libjpeg")]

use flpdf_libjpeg_compat::{decode_scanlines, DecodeError};

#[test]
fn malformed_jpeg_returns_codec_diagnostic_without_callback() {
    let mut callback_count = 0;
    let result = decode_scanlines(b"not a jpeg", &mut |_row| {
        callback_count += 1;
        Ok::<(), ()>(())
    });

    match result {
        Err(DecodeError::Codec(message)) => {
            assert_eq!(message, "Not a JPEG file: starts with 0x6e 0x6f");
        }
        Err(error) => panic!("unexpected error: {error:?}"),
        Ok(()) => panic!("malformed JPEG must fail"),
    }
    assert_eq!(callback_count, 0);
}

#[test]
fn empty_input_reports_qpdf_whole_buffer_exhaustion() {
    let result = decode_scanlines(&[], &mut |_row| Ok::<(), ()>(()));

    assert!(matches!(
        result,
        Err(DecodeError::Codec(message))
            if message == "invalid jpeg data reading from buffer"
    ));
}
