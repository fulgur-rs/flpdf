//! qpdf correspondence: `QPDFJob::doShowObj`, `doShowPages`, and object/stream inspection helpers (`libqpdf/QPDFJob.cc:805-874`).
//!
//! The CLI owns argument parsing, while this module owns the live
//! `ObjectHandle` inspection and the shared job completion boundary. Keeping
//! this code here prevents an external binary crate from falling back to the
//! legacy `Object` materialization route for read-only inspection.

use super::lifecycle::{JobExitCode, QPDFJob};
use crate::writer::DecodeLevel;
use crate::{Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf, Result};
use std::io::{Read, Seek};

impl QPDFJob {
    /// Dump one indirect object using qpdf's resolved object syntax.
    pub fn dump_object<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        object_ref: ObjectRef,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            let object = pdf.get_object_handle(object_ref);
            ensure_present(&object, object_ref)?;
            let mut output = unparse_object_with_stream_data(pdf, &object, object_ref)?;
            output.push(b'\n');
            logger.info(output)
        })
    }

    /// Show one object through the canonical qpdf object/stream boundary.
    pub fn show_object<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        object: ObjectHandle,
        raw_stream_data: bool,
        filtered_stream_data: bool,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            object.type_code()?;
            if object.as_stream_dict().is_some() {
                if raw_stream_data || filtered_stream_data {
                    let warning_count = pdf.repair_diagnostics().entries().len();
                    let data_result = if filtered_stream_data {
                        object.get_stream_data(DecodeLevel::All)
                    } else {
                        object.get_raw_stream_data()
                    };
                    let data = match data_result {
                        Ok(data) => data,
                        Err(_error) if pdf.repair_diagnostics().entries().len() > warning_count => {
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    };
                    write_to_standard_output(&logger, data.as_ref())
                } else {
                    let dictionary = object
                        .as_stream_dict()
                        .expect("stream type code guarantees a stream dictionary");
                    let mut output = b"Object is stream.  Dictionary:\n".to_vec();
                    output.extend_from_slice(&dictionary.unparse_resolved());
                    output.push(b'\n');
                    logger.info(output)
                }
            } else {
                let mut output = object.unparse_resolved();
                output.push(b'\n');
                logger.info(output)
            }
        })
    }

    /// Show one stream's raw or filtered data through qpdf's stream handle.
    pub fn show_stream<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        object_ref: ObjectRef,
        raw_stream_data: bool,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            let object = pdf.get_object_handle(object_ref);
            ensure_present(&object, object_ref)?;
            object.type_code()?;
            let Some(stream_dictionary) = object.as_stream_dict() else {
                return Err(Error::Unsupported(format!(
                    "object {} {} R is not a stream",
                    object_ref.number, object_ref.generation
                )));
            };

            if raw_stream_data {
                let raw = object.get_raw_stream_data()?;
                let bytes = cli_stream_bytes(pdf, object_ref, &object, raw.as_ref(), true)?;
                return write_to_standard_output(&logger, bytes);
            }

            // Preserve the existing CLI marker for the specialized codecs that
            // qpdf keeps as raw data in this command. The name is read from the
            // live stream dictionary; no filter dictionary is materialized.
            if let Some(filter_name) = first_stream_filter_name(&stream_dictionary)? {
                if !crate::filters::is_decoded_filter(&filter_name) {
                    if let Some(label) = crate::filters::passthrough_codec_label(&filter_name) {
                        let raw = object.get_raw_stream_data()?;
                        let len =
                            cli_stream_bytes(pdf, object_ref, &object, raw.as_ref(), true)?.len();
                        return logger.info(format!("<binary, {len} bytes, codec {label}>\n"));
                    }
                }
            }

            let unfiltered = stream_is_unfiltered(&stream_dictionary)?;
            let decoded = object.get_stream_data(DecodeLevel::All)?;
            let bytes = cli_stream_bytes(pdf, object_ref, &object, decoded.as_ref(), unfiltered)?;
            write_to_standard_output(&logger, bytes)
        })
    }

    /// Show the number of repaired page leaves in document order.
    pub fn show_npages<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
            logger.info(format!("{}\n", pages.len()))
        })
    }

    /// Show the CLI page description through the page/job ObjectHandle route.
    pub fn show_pages<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
            for (index, page_ref) in page_refs.iter().enumerate() {
                let (media_box, resources, contents, rotate) = {
                    let mut page = PageObjectHelper::new(*page_ref, pdf);
                    (
                        page.get_attribute(b"/MediaBox", false)?,
                        page.get_attribute(b"/Resources", false)?,
                        page.get_attribute(b"/Contents", false)?,
                        page.get_attribute(b"/Rotate", false)?,
                    )
                };

                logger.info(format!("page {}: {}\n", index + 1, page_ref))?;
                write_page_attribute(&logger, "media-box", &media_box)?;
                write_page_attribute(&logger, "resources", &resources)?;
                write_page_attribute(&logger, "contents", &contents)?;
                write_page_attribute(&logger, "rotate", &rotate)?;
            }
            Ok(())
        })
    }
}

fn ensure_present(object: &ObjectHandle, object_ref: ObjectRef) -> Result<()> {
    object.type_code()?;
    if object.is_null() {
        // A missing object reference has no qpdf counterpart to preserve --
        // qpdf's own doShowObj (QPDFJob.cc:806-840) never rejects one; it
        // simply unparses the null object handle and succeeds. This is
        // flpdf's own pre-existing hard-stop, kept as-is by this migration
        // (Error::Unsupported would prepend "unsupported PDF feature: ",
        // changing the diagnostic text this route has always emitted).
        return Err(Error::System(format!(
            "object {} {} R not found",
            object_ref.number, object_ref.generation
        )));
    }
    Ok(())
}

fn unparse_object_with_stream_data<R: Read + Seek>(
    pdf: &Pdf<R>,
    object: &ObjectHandle,
    object_ref: ObjectRef,
) -> Result<Vec<u8>> {
    if let Some(dictionary) = object.as_stream_dict() {
        let data = object.get_raw_stream_data()?;
        let mut output = dictionary.unparse_resolved();
        output.extend_from_slice(b"\nstream\n");
        let data = data.as_ref();
        let recovered_eol = pdf.canonical_recovered_stream_eol(object_ref, object)?;
        let data = if let Some(eol) = recovered_eol.filter(|eol| data.ends_with(eol)) {
            &data[..data.len() - eol.len()]
        } else {
            data
        };
        output.extend_from_slice(data);
        output.extend_from_slice(b"\nendstream");
        Ok(output)
    } else {
        Ok(object.unparse_resolved())
    }
}

fn first_stream_filter_name(stream_dictionary: &ObjectHandle) -> Result<Option<Vec<u8>>> {
    let filter = stream_dictionary.get_key(b"/Filter");
    filter.type_code()?;
    if let Some(name) = filter.as_name() {
        return Ok(Some(name));
    }
    let Some(items) = filter.as_array() else {
        return Ok(None);
    };
    if items.len() != 1 {
        return Ok(None);
    }
    let item = items
        .into_iter()
        .next()
        .expect("one-element filter array has one item");
    item.type_code()?;
    Ok(item.as_name())
}

fn stream_is_unfiltered(stream_dictionary: &ObjectHandle) -> Result<bool> {
    let filter = stream_dictionary.get_key(b"/Filter");
    filter.type_code()?;
    if filter.is_null() {
        return Ok(true);
    }
    // An empty `/Filter []` array applies zero filters, same as a missing
    // `/Filter` (QPDF_Stream.cc:396-406: the per-item loop over an empty
    // array leaves `filter_names` empty, so decoding is a no-op).
    Ok(filter.as_array().is_some_and(|items| items.is_empty()))
}

fn cli_stream_bytes<'a, R: Read + Seek>(
    pdf: &Pdf<R>,
    object_ref: ObjectRef,
    stream: &ObjectHandle,
    data: &'a [u8],
    trim_recovered_eol: bool,
) -> Result<&'a [u8]> {
    let Some(eol) = pdf.canonical_recovered_stream_eol(object_ref, stream)? else {
        return Ok(data);
    };
    Ok(if trim_recovered_eol && data.ends_with(eol) {
        &data[..data.len() - eol.len()]
    } else {
        data
    })
}

fn write_to_standard_output(logger: &crate::QPDFLogger, data: &[u8]) -> Result<()> {
    logger.save_to_standard_output(true)?;
    logger.get_save()?.write(data).map_err(Error::from)
}

fn write_page_attribute(
    logger: &crate::QPDFLogger,
    label: &str,
    value: &ObjectHandle,
) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    let rendered = String::from_utf8_lossy(&value.unparse()).into_owned();
    logger.info(format!("  {label}: {rendered}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_job() -> QPDFJob {
        let logger = crate::QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job
    }

    fn recovered_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut bytes = b"%PDF-1.4\n1 0 obj\n<< >>\nstream\nabc\nendstream\nendobj\n".to_vec();
        let xref = bytes.len();
        bytes.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        bytes.extend_from_slice(b"0000000009 00000 n \n");
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        Pdf::open_mem_owned(bytes).expect("recovered stream fixture should open")
    }

    fn stream() -> ObjectHandle {
        ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"JBIG2Decode".to_vec()),
            )]),
            std::rc::Rc::new(b"encoded".to_vec()),
        )
    }

    #[test]
    fn first_stream_filter_name_reads_direct_and_single_item_array() {
        let direct = stream().as_stream_dict().expect("stream dictionary");
        assert_eq!(
            first_stream_filter_name(&direct).unwrap(),
            Some(b"JBIG2Decode".to_vec())
        );

        let array = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::name(b"JPXDecode".to_vec())]),
            )]),
            std::rc::Rc::new(Vec::new()),
        );
        assert_eq!(
            first_stream_filter_name(&array.as_stream_dict().unwrap()).unwrap(),
            Some(b"JPXDecode".to_vec())
        );
    }

    #[test]
    fn first_stream_filter_name_ignores_missing_and_multi_item_filters() {
        let missing = ObjectHandle::dictionary(Vec::new());
        assert_eq!(first_stream_filter_name(&missing).unwrap(), None);

        let multiple = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"ASCII85Decode".to_vec()),
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                ]),
            )]),
            std::rc::Rc::new(Vec::new()),
        );
        assert_eq!(
            first_stream_filter_name(&multiple.as_stream_dict().unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn show_object_returns_unwarned_filter_errors() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"UnknownFilter".to_vec()),
            )]),
            std::rc::Rc::new(b"encoded".to_vec()),
        );
        let error = quiet_job()
            .show_object(&mut pdf, stream, false, true)
            .expect_err("an unknown filter must remain an operation error");
        assert!(error.to_string().contains("decoded stream data"));
    }

    #[test]
    fn show_stream_returns_an_unknown_filter_error_after_marker_probe() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let stream_ref = ObjectRef::new(7, 0);
        let stream = pdf.get_object_handle(stream_ref);
        pdf.resolve_object_handle(&stream).unwrap();
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Filter", ObjectHandle::name(b"UnknownFilter".to_vec()))
            .unwrap();
        let error = quiet_job()
            .show_stream(&mut pdf, stream_ref, false)
            .expect_err("an unknown filter must remain an operation error");
        assert!(error.to_string().contains("decoded stream data"));
    }

    #[test]
    fn unparse_stream_keeps_source_bytes_without_recovered_framing() {
        let pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let output =
            unparse_object_with_stream_data(&pdf, &stream(), ObjectRef::new(7, 0)).unwrap();
        assert!(output.ends_with(b"encoded\nendstream"));
    }

    #[test]
    fn cli_stream_bytes_keeps_data_when_recovered_eol_is_not_to_be_trimmed() {
        let mut pdf = recovered_pdf();
        let object_ref = ObjectRef::new(1, 0);
        let handle = pdf.get_object_handle(object_ref);
        handle.get_raw_stream_data().unwrap();
        let data = b"decoded\n";
        assert_eq!(
            cli_stream_bytes(&pdf, object_ref, &handle, data, false).unwrap(),
            data
        );
    }

    #[test]
    fn write_page_attribute_skips_null_and_renders_handles() {
        let logger = crate::QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        write_page_attribute(&logger, "missing", &ObjectHandle::null()).unwrap();
        write_page_attribute(&logger, "value", &ObjectHandle::integer(7)).unwrap();
    }
}
