use flpdf::filters::{decode_stream_data_recovering_with_limits, DecodeLimits};
use flpdf::ObjectHandle;

fn wide_tiff_stream_dictionary() -> ObjectHandle {
    ObjectHandle::dictionary(vec![
        (
            b"/Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        ),
        (
            b"/DecodeParms".to_vec(),
            ObjectHandle::dictionary(vec![
                (b"/Predictor".to_vec(), ObjectHandle::integer(2)),
                (b"/Columns".to_vec(), ObjectHandle::integer(536_870_911)),
                (b"/Colors".to_vec(), ObjectHandle::integer(1)),
                (b"/BitsPerComponent".to_vec(), ObjectHandle::integer(8)),
            ]),
        ),
    ])
}

#[test]
fn decode_limits_reject_tiff_memory_before_partial_row_padding() {
    let limits = DecodeLimits {
        max_output: Some(0),
        max_tiff_memory: Some(1 << 20),
        ..DecodeLimits::default()
    };

    let error =
        decode_stream_data_recovering_with_limits(&wide_tiff_stream_dictionary(), &[], limits)
            .expect_err("the configured TIFF row budget must reject the geometry");
    assert!(error
        .to_string()
        .contains("TIFFPredictor memory limit exceeded"));
}

#[test]
fn zero_tiff_memory_limit_preserves_unlimited_qpdf_default() {
    let limits = DecodeLimits {
        max_output: Some(0),
        max_tiff_memory: Some(0),
        ..DecodeLimits::default()
    };

    let output =
        decode_stream_data_recovering_with_limits(&wide_tiff_stream_dictionary(), &[], limits)
            .expect("zero TIFF memory limit disables the optional hardening");
    assert!(output.data.is_empty());
}
