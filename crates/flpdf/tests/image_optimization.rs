//! Canonical library coverage for qpdf's image optimizer.

use flpdf::{
    optimize_images, ImageOptimizationOptions, PageDocumentHelper, PageObjectHelper, Pdf,
    QPDFLogger,
};
use std::io::Cursor;

fn stream_object(dictionary: &[u8], data: &[u8]) -> Vec<u8> {
    let mut object = dictionary.to_vec();
    object.extend_from_slice(b"\nstream\n");
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream");
    object
}

fn optimizer_fixture() -> Vec<u8> {
    let large_gray = vec![128u8; 40_000];
    let tiny_gray = vec![128u8; 1];
    let image_dictionary = |extra: &str, length: usize| {
        format!(
            "<< /Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length {length} {extra} >>"
        )
        .into_bytes()
    };
    let objects = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Good 5 0 R /Small 6 0 R /BadBits 7 0 R /BadColor 8 0 R /Missing 9 0 R /NoColorName 10 0 R /Dct 11 0 R >> >> /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_object(b"<< /Length 0 >>", b"")),
        (5, stream_object(&image_dictionary("", large_gray.len()), &large_gray)),
        (
            6,
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>",
                &tiny_gray,
            ),
        ),
        (
            7,
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 1 /Length 40000 >>",
                &large_gray,
            ),
        ),
        (
            8,
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace /Pattern /BitsPerComponent 8 /Length 40000 >>",
                &large_gray,
            ),
        ),
        (
            9,
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 200 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>",
                &tiny_gray,
            ),
        ),
        (
            10,
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace 1 /BitsPerComponent 8 /Length 40000 >>",
                &large_gray,
            ),
        ),
        (
            11,
            stream_object(
                b"<< /Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Length 3 >>",
                b"bad",
            ),
        ),
    ];

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (number, body) in objects {
        offsets.push((number, pdf.len()));
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(&body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 12\n0000000000 65535 f \n");
    for (_, offset) in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 12 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
    pdf
}

fn resource_image<R: std::io::Read + std::io::Seek + 'static>(
    pdf: &mut Pdf<R>,
    key: &[u8],
) -> flpdf::ObjectHandle {
    let page_ref = PageDocumentHelper::new(pdf)
        .get_all_pages()
        .expect("fixture page")
        .into_iter()
        .next()
        .expect("one page");
    let mut page = PageObjectHelper::new(page_ref, pdf);
    let resources = page.get_resources(false).expect("page resources");
    drop(page);
    let xobjects = resources
        .try_get_key(b"/XObject")
        .expect("XObject resources");
    let image = xobjects.try_get_key(key).expect("image resource");
    pdf.resolve(&image).expect("image object");
    image
}

fn filter_name<R: std::io::Read + std::io::Seek + 'static>(
    pdf: &mut Pdf<R>,
    key: &[u8],
) -> Option<Vec<u8>> {
    let image = resource_image(pdf, key);
    let dictionary = image.as_stream_dict().expect("image dictionary");
    let filter = dictionary.try_get_key(b"/Filter").expect("filter key");
    pdf.resolve(&filter).expect("filter value");
    filter.as_name()
}

#[test]
fn optimizer_matches_qpdf_metadata_decisions_and_installs_lazy_jpeg() {
    let bytes = optimizer_fixture();
    let logger = QPDFLogger::create();
    logger.set_info(Some(logger.discard()));
    let mut pdf = Pdf::open(Cursor::new(bytes.clone())).expect("fixture PDF");
    let options = ImageOptimizationOptions {
        min_width: 0,
        min_height: 0,
        min_area: 0,
        ..ImageOptimizationOptions::default()
    };

    optimize_images(&mut pdf, &logger, "qpdf", true, options).expect("optimize fixture");

    assert_eq!(filter_name(&mut pdf, b"/Good"), Some(b"DCTDecode".to_vec()));
    assert_eq!(filter_name(&mut pdf, b"/Small"), None);
    assert_eq!(filter_name(&mut pdf, b"/BadBits"), None);
    assert_eq!(filter_name(&mut pdf, b"/BadColor"), None);
    assert_eq!(filter_name(&mut pdf, b"/Missing"), None);
    assert_eq!(filter_name(&mut pdf, b"/NoColorName"), None);
    assert_eq!(filter_name(&mut pdf, b"/Dct"), Some(b"DCTDecode".to_vec()));

    let good = resource_image(&mut pdf, b"/Good");
    let jpeg = good.get_raw_stream_data().expect("lazy JPEG provider");
    assert!(jpeg.starts_with(&[0xff, 0xd8]));

    let mut keep_inline_pdf = Pdf::open(Cursor::new(bytes)).expect("fixture PDF");
    let mut keep_options = options;
    keep_options.keep_inline_images = true;
    optimize_images(&mut keep_inline_pdf, &logger, "qpdf", true, keep_options)
        .expect("optimize fixture with inline images kept");
}
