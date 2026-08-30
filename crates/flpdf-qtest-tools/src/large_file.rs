//! Rust port of qpdf 11.9.0's `qpdf/test_large_file.cc` helper.
//!
//! The helper deliberately exercises the production lazy stream-provider and
//! page-document/writer paths. It does not build a second PDF representation
//! or route around `flpdf`; the only state retained by the image provider is
//! one reusable stripe buffer, matching qpdf's bounded provider writes.

use flpdf::{
    DecodeLevel, Error, ObjectHandle, PageDocumentHelper, PageInput, Pdf, PdfWriter, Pipeline,
    PipelineResult, StreamDataMode, StreamDataProvider,
};
use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::rc::Rc;

const NSTRIPES: usize = 10;
const STRIPESIZE_LARGE: usize = 500;
const STRIPESIZE_SMALL: usize = 5;
const NPAGES: usize = 200;

#[derive(Debug)]
pub struct HelperError {
    pub output: Vec<u8>,
    pub message: String,
}

#[derive(Clone)]
struct Output(Rc<RefCell<Vec<u8>>>);

impl Output {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(Vec::new())))
    }

    fn append(&self, arguments: std::fmt::Arguments<'_>) {
        self.0.borrow_mut().write_fmt(arguments).unwrap();
    }

    fn into_bytes(self) -> Vec<u8> {
        Rc::try_unwrap(self.0)
            .expect("large-file output has no remaining provider references")
            .into_inner()
    }
}

struct ImageProvider {
    page: usize,
    width: usize,
    stripesize: usize,
    output: Output,
    // qpdf's `test_large_file.cc:47,120-121` allocates `buf` as a single
    // namespace-scope pointer shared by every `ImageProvider` instance
    // across all `npages`, not a per-page buffer. Share the same allocation
    // across every provider the same way, rather than retaining one stripe
    // buffer per page (200 buffers x up to 500 rows in `large` mode).
    stripe: Rc<RefCell<Option<Vec<u8>>>>,
}

impl StreamDataProvider for ImageProvider {
    fn provide_stream_data(
        &self,
        _object_ref: flpdf::ObjectRef,
        pipeline: &mut dyn Pipeline,
    ) -> flpdf::Result<()> {
        let mut stripe = self.stripe.borrow_mut();
        let stripe = stripe.get_or_insert_with(|| vec![0; self.width * self.stripesize]);
        self.output
            .append(format_args!("page {} of {}\n", self.page, NPAGES));
        for row in 0..NSTRIPES {
            let color = pixel_color(self.page, row);
            stripe.fill(color);
            pipeline.write(stripe).map_err(Error::from)?;
        }
        pipeline.finish().map_err(Error::from)
    }
}

struct ImageChecker {
    page: usize,
    width: usize,
    stripesize: usize,
    offset: usize,
    okay: bool,
    output: Output,
}

impl Pipeline for ImageChecker {
    fn identifier(&self) -> &str {
        "image checker"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        for (index, byte) in data.iter().copied().enumerate() {
            let row = (self.offset + index) / self.width / self.stripesize;
            if byte != pixel_color(self.page, row) {
                self.okay = false;
            }
        }
        self.offset += data.len();
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        if !self.okay {
            self.output.append(format_args!(
                "errors found checking image data for page {}\n",
                self.page
            ));
        }
        Ok(())
    }
}

fn pixel_color(page: usize, row: usize) -> u8 {
    if page & (1usize << (NSTRIPES - 1 - row)) != 0 {
        0xc0
    } else {
        0x40
    }
}

fn generate_page_contents(page: usize) -> Vec<u8> {
    format!("BT /F1 24 Tf 72 720 Td (page {page}) Tj ET\nq 468 0 0 468 72 72 cm /Im1 Do Q\n")
        .into_bytes()
}

fn usage(argv0: &OsString) -> HelperError {
    let name = argv0.to_string_lossy();
    HelperError {
        output: Vec::new(),
        message: format!("Usage: {name} {{read|write}} {{large|small}} outfile"),
    }
}

fn parse_args(args: &[OsString]) -> Result<(bool, bool, &Path), HelperError> {
    if args.len() != 4 {
        return Err(usage(&args[0]));
    }
    let write_mode = match args[1].to_str() {
        Some("write") => true,
        Some("read") => false,
        _ => return Err(usage(&args[0])),
    };
    let large = match args[2].to_str() {
        Some("large") => true,
        Some("small") => false,
        _ => return Err(usage(&args[0])),
    };
    Ok((write_mode, large, Path::new(&args[3])))
}

fn parameters(large: bool) -> (usize, usize) {
    let stripesize = if large {
        STRIPESIZE_LARGE
    } else {
        STRIPESIZE_SMALL
    };
    let width = NSTRIPES * stripesize;
    (width, stripesize)
}

fn create_pdf(path: &Path, large: bool, output: &Output) -> flpdf::Result<()> {
    let (width, stripesize) = parameters(large);
    let mut pdf = Pdf::empty()?;

    let font = pdf.make_indirect_object_handle(ObjectHandle::dictionary(vec![
        (b"Type".to_vec(), ObjectHandle::name(b"Font".to_vec())),
        (b"Subtype".to_vec(), ObjectHandle::name(b"Type1".to_vec())),
        (b"Name".to_vec(), ObjectHandle::name(b"F1".to_vec())),
        (
            b"BaseFont".to_vec(),
            ObjectHandle::name(b"Helvetica".to_vec()),
        ),
        (
            b"Encoding".to_vec(),
            ObjectHandle::name(b"WinAnsiEncoding".to_vec()),
        ),
    ]))?;
    let procset = pdf.make_indirect_object_handle(ObjectHandle::array(vec![
        ObjectHandle::name(b"PDF".to_vec()),
        ObjectHandle::name(b"Text".to_vec()),
        ObjectHandle::name(b"ImageC".to_vec()),
    ]))?;
    let rfont = ObjectHandle::dictionary(vec![(b"F1".to_vec(), font)]);
    let mediabox = ObjectHandle::array(vec![
        ObjectHandle::integer(0),
        ObjectHandle::integer(0),
        ObjectHandle::integer(612),
        ObjectHandle::integer(792),
    ]);
    let stripe: Rc<RefCell<Option<Vec<u8>>>> = Rc::new(RefCell::new(None));

    for page_number in 1..=NPAGES {
        let image = pdf.new_stream()?;
        let image_dict = image
            .as_stream_dict()
            .ok_or_else(|| Error::Internal("new image is not a stream".to_owned()))?;
        image_dict.replace_key(b"/Type", ObjectHandle::name(b"XObject".to_vec()))?;
        image_dict.replace_key(b"/Subtype", ObjectHandle::name(b"Image".to_vec()))?;
        image_dict.replace_key(b"/ColorSpace", ObjectHandle::name(b"DeviceGray".to_vec()))?;
        image_dict.replace_key(b"/BitsPerComponent", ObjectHandle::integer(8))?;
        image_dict.replace_key(b"/Width", ObjectHandle::integer(width as i64))?;
        image_dict.replace_key(
            b"/Height",
            ObjectHandle::integer((NSTRIPES * stripesize) as i64),
        )?;
        image.replace_stream_data_provider(
            Rc::new(ImageProvider {
                page: page_number,
                width,
                stripesize,
                output: output.clone(),
                stripe: stripe.clone(),
            }),
            Some(ObjectHandle::null()),
            Some(ObjectHandle::null()),
        )?;
        pdf.mark_object_handle_dirty(&image)?;

        let xobject = ObjectHandle::dictionary(vec![(b"Im1".to_vec(), image)]);
        let resources = ObjectHandle::dictionary(vec![
            (b"ProcSet".to_vec(), procset.clone()),
            (b"Font".to_vec(), rfont.clone()),
            (b"XObject".to_vec(), xobject),
        ]);
        let contents = pdf.new_stream_with_data(Rc::new(generate_page_contents(page_number)))?;
        let page = pdf.make_indirect_object_handle(ObjectHandle::dictionary(vec![
            (b"Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (b"MediaBox".to_vec(), mediabox.clone()),
            (b"Contents".to_vec(), contents),
            (b"Resources".to_vec(), resources),
        ]))?;
        let page_ref = page
            .object_ref()
            .ok_or_else(|| Error::Internal("new page is not indirect".to_owned()))?;
        PageDocumentHelper::new(&mut pdf).add_page(
            PageInput::<'_, std::io::Cursor<Vec<u8>>>::Existing(page_ref),
            false,
        )?;
    }

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_static_id(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.set_object_stream_mode(flpdf::ObjectStreamMode::Disable);
    writer.set_output_file(path)?;
    writer.write()
}

fn check_page_contents<R: Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page: flpdf::ObjectRef,
    page_number: usize,
    output: &Output,
) -> flpdf::Result<()> {
    let page_handle = pdf.get_object_handle(page);
    let contents = page_handle
        .get_key(b"/Contents")
        .get_stream_data(DecodeLevel::Generalized)?;
    let expected = generate_page_contents(page_number);
    if contents.as_slice() != expected.as_slice() {
        output.append(format_args!(
            "page contents wrong for page {page_number}\nACTUAL: {}EXPECTED: {}----\n",
            String::from_utf8_lossy(contents.as_slice()),
            String::from_utf8_lossy(&expected)
        ));
    }
    Ok(())
}

fn check_image<R: Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page: flpdf::ObjectRef,
    page_number: usize,
    stripesize: usize,
    output: &Output,
) -> flpdf::Result<()> {
    let page_handle = pdf.get_object_handle(page);
    let image = page_handle
        .get_key(b"/Resources")
        .get_key(b"/XObject")
        .get_key(b"/Im1");
    let mut checker = ImageChecker {
        page: page_number,
        width: NSTRIPES * stripesize,
        stripesize,
        offset: 0,
        okay: true,
        output: output.clone(),
    };
    let mut filtering_attempted = false;
    if !image.pipe_stream_data(
        &mut checker,
        &mut filtering_attempted,
        0,
        DecodeLevel::Specialized,
        false,
        false,
    )? {
        return Err(Error::Unsupported(format!(
            "image stream for page {page_number} could not be read"
        )));
    }
    Ok(())
}

fn check_pdf(path: &Path, large: bool, output: &Output) -> flpdf::Result<()> {
    let (_, stripesize) = parameters(large);
    let file = File::open(path).map_err(|error| Error::FileIo {
        operation: "open",
        path: path.to_path_buf(),
        source: error,
    })?;
    let mut pdf = Pdf::open_with_options(
        BufReader::new(file),
        flpdf::PdfOpenOptions {
            description: path.display().to_string(),
            ..flpdf::PdfOpenOptions::default()
        },
    )?;
    let pages = PageDocumentHelper::new(&mut pdf).get_all_pages()?;
    if pages.len() != NPAGES {
        return Err(Error::Unsupported(format!(
            "expected {NPAGES} pages, found {}",
            pages.len()
        )));
    }
    for (index, page) in pages.into_iter().enumerate() {
        let page_number = index + 1;
        output.append(format_args!("page {page_number} of {NPAGES}\n"));
        check_page_contents(&mut pdf, page, page_number, output)?;
        check_image(&mut pdf, page, page_number, stripesize, output)?;
    }
    Ok(())
}

pub fn run(args: &[OsString]) -> Result<Vec<u8>, HelperError> {
    let (write_mode, large, path) = parse_args(args)?;
    let output = Output::new();
    let operation = if write_mode {
        create_pdf(path, large, &output)
    } else {
        check_pdf(path, large, &output)
    };
    match operation {
        Ok(()) => Ok(output.into_bytes()),
        Err(error) => Err(HelperError {
            output: output.into_bytes(),
            message: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parameters_match_qpdf_small_and_large_modes() {
        assert_eq!(parameters(false), (50, 5));
        assert_eq!(parameters(true), (5000, 500));
    }

    #[test]
    fn generated_page_content_is_unique_and_qpdf_shaped() {
        assert_eq!(
            generate_page_contents(1),
            b"BT /F1 24 Tf 72 720 Td (page 1) Tj ET\nq 468 0 0 468 72 72 cm /Im1 Do Q\n"
        );
        assert_ne!(generate_page_contents(1), generate_page_contents(NPAGES));
    }

    #[test]
    fn invalid_arguments_return_qpdf_usage_without_output() {
        let args = vec![OsString::from("test_large_file")];
        let error = run(&args).expect_err("invalid argv");
        assert!(error.message.starts_with("Usage: test_large_file"));
        assert!(error.output.is_empty());

        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("inspect"),
            OsString::from("small"),
            OsString::from("out.pdf"),
        ];
        assert!(run(&args).is_err(), "bad operation must use qpdf usage");

        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("read"),
            OsString::from("medium"),
            OsString::from("out.pdf"),
        ];
        assert!(run(&args).is_err(), "bad size must use qpdf usage");
    }

    #[test]
    fn missing_and_malformed_inputs_are_reported_at_the_file_boundary() {
        let missing = tempfile::tempdir().unwrap().path().join("missing.pdf");
        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("read"),
            OsString::from("small"),
            missing.into_os_string(),
        ];
        let missing_error = run(&args).expect_err("missing input");
        assert!(missing_error.message.contains("open"));

        let directory = tempfile::tempdir().unwrap();
        let malformed = directory.path().join("malformed.pdf");
        std::fs::write(&malformed, b"not a PDF").unwrap();
        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("read"),
            OsString::from("small"),
            malformed.into_os_string(),
        ];
        let malformed_error = run(&args).expect_err("malformed input");
        assert!(!malformed_error.message.is_empty());
    }

    #[test]
    fn content_and_image_mismatches_use_qpdf_helper_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large-file.pdf");
        let generation_output = Output::new();
        create_pdf(&path, false, &generation_output).expect("write helper PDF");

        let mut pdf = Pdf::open(BufReader::new(File::open(&path).unwrap())).unwrap();
        let page = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let output = Output::new();
        check_page_contents(&mut pdf, page, 2, &output).unwrap();
        let mut checker = ImageChecker {
            page: 512,
            width: 1,
            stripesize: 1,
            offset: 0,
            okay: true,
            output: output.clone(),
        };
        Pipeline::write(&mut checker, &[0x40]).unwrap();
        Pipeline::finish(&mut checker).unwrap();
        drop(checker);
        let diagnostics = String::from_utf8(output.into_bytes()).unwrap();
        assert!(diagnostics.contains("page contents wrong for page 2"));
        assert!(diagnostics.contains("errors found checking image data for page 512"));
    }

    #[test]
    fn output_open_failures_return_status_two_without_a_partial_success() {
        let directory = tempfile::tempdir().unwrap();
        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("write"),
            OsString::from("small"),
            directory.path().as_os_str().to_os_string(),
        ];
        let error = run(&args).expect_err("a directory cannot be a PDF output");
        assert!(error.output.is_empty());
        assert!(!error.message.is_empty());
    }

    #[test]
    fn write_and_read_small_generated_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = PathBuf::from(directory.path()).join("large-file.pdf");
        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("write"),
            OsString::from("small"),
            path.clone().into_os_string(),
        ];
        let write_output = run(&args).expect("write helper PDF");
        assert_eq!(
            write_output
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            NPAGES
        );

        let args = vec![
            OsString::from("test_large_file"),
            OsString::from("read"),
            OsString::from("small"),
            path.into_os_string(),
        ];
        let read_output = run(&args).expect("read helper PDF");
        assert_eq!(write_output, read_output);
    }
}
