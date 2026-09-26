//! The CLI owns argument parsing, while this module owns the live
//! `ObjectHandle` inspection and the shared job completion boundary. Keeping
//! this code here prevents an external binary crate from falling back to the
//! legacy `Object` materialization route for read-only inspection.
//!
//! qpdf correspondence: `QPDFJob::doShowObj`, `doShowPages`, and object/stream inspection helpers (`libqpdf/QPDFJob.cc:805-874`).
//!
//!

use super::lifecycle::{JobExitCode, QPDFJob};
use crate::writer::DecodeLevel;
use crate::{Error, ObjectHandle, PageDocumentHelper, PageObjectHelper, Pdf, Result, XrefEntry};
use std::io::{Read, Seek};

impl QPDFJob {
    /// Show one object through the canonical qpdf object/stream boundary.
    pub fn show_object<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        object: ObjectHandle,
        raw_stream_data: bool,
        filtered_stream_data: bool,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        let normalize_content = self.content_normalization_enabled();
        self.inspect(pdf, |pdf| {
            emit_show_object(
                pdf,
                &logger,
                &object,
                raw_stream_data,
                filtered_stream_data,
                normalize_content,
            )
        })
    }

    /// Emit one object report without completing the enclosing job.
    pub(crate) fn show_object_report<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        object: &ObjectHandle,
        raw_stream_data: bool,
        filtered_stream_data: bool,
    ) -> Result<()> {
        let logger = self.logger();
        let normalize_content = self.content_normalization_enabled();
        emit_show_object(
            pdf,
            &logger,
            object,
            raw_stream_data,
            filtered_stream_data,
            normalize_content,
        )
    }

    /// Show the raw `/Pages /Count` value from the catalog.
    ///
    /// This is qpdf's `QPDFJob::doInspection` `--show-npages` boundary
    /// (`libqpdf/QPDFJob.cc:1646-1655`), not a page-tree enumeration. qpdf
    /// deliberately reads `/Pages` and then `/Count` through generic
    /// `QPDFObjectHandle` accessors, so a present but inconsistent count is
    /// printed verbatim and a missing/malformed key produces qpdf's warning
    /// plus the integer accessor's zero fallback.
    pub fn show_npages<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| emit_npages(pdf, &logger))
    }

    /// Emit the page-count report without completing the enclosing job.
    ///
    /// This is used by the job-JSON inspection dispatcher so qpdf's one-shot
    /// completion boundary is retained when several inspection flags are set.
    pub(crate) fn show_npages_report<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<()> {
        let logger = self.logger();
        emit_npages(pdf, &logger)
    }

    /// Show qpdf's raw cross-reference table through the inspection lifecycle
    /// (`QPDF::showXRefTable`, `libqpdf/QPDF.cc:1213-1240`). The reader-owned
    /// raw snapshot preserves `QPDFObjGen` identity before the stricter
    /// `ObjectRef` parser boundary.
    pub fn show_xref<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| emit_xref(pdf, &logger))
    }

    /// Emit the xref report without completing the enclosing job.
    pub(crate) fn show_xref_report<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<()> {
        let logger = self.logger();
        emit_xref(pdf, &logger)
    }

    /// Show pages through qpdf's `QPDFJob::doShowPages` route.
    ///
    /// The output contains only page identity, optional direct image details,
    /// and `/Contents` stream references, matching
    /// `libqpdf/QPDFJob.cc:842-874`. In particular, effective inheritable
    /// attributes such as `/MediaBox` and `/Rotate` are not part of qpdf's
    /// `--show-pages` output.
    pub fn show_pages<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        let show_page_images = self.show_page_images();
        self.inspect(pdf, |pdf| emit_show_pages(pdf, &logger, show_page_images))
    }

    /// Emit qpdf's page report without completing the enclosing job.
    pub(crate) fn show_pages_report_with_images<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        show_page_images: bool,
    ) -> Result<()> {
        let logger = self.logger();
        emit_show_pages(pdf, &logger, show_page_images)
    }
}

fn emit_xref<R: Read + Seek>(pdf: &mut Pdf<R>, logger: &crate::QPDFLogger) -> Result<()> {
    for (object_ref, entry) in pdf.get_raw_xref_table() {
        // qpdf writes `og.unparse('/') << ": "` unconditionally before
        // switching on the entry type (`QPDF.cc:1219-1220`), so that partial
        // prefix reaches stdout even for the `default` arm's
        // `std::logic_error` below -- it is not held back until a complete
        // line is ready.
        logger.info(format!(
            "{}/{}: ",
            object_ref.get_obj(),
            object_ref.get_gen()
        ))?; // cov:ignore: multiline call terminator has no executable coverage region
        let line = match entry {
            XrefEntry::Free { .. } => {
                return Err(Error::Internal(
                    "unknown cross-reference table type while showing xref_table".to_owned(),
                ));
            }
            XrefEntry::Uncompressed { offset } => format!("uncompressed; offset = {offset}\n"),
            XrefEntry::Compressed { stream, index } => {
                format!("compressed; stream = {stream}, index = {index}\n")
            }
        };
        logger.info(line)?;
    }
    Ok(())
}

fn emit_show_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    logger: &crate::QPDFLogger,
    object: &ObjectHandle,
    raw_stream_data: bool,
    filtered_stream_data: bool,
    normalize_content: bool,
) -> Result<()> {
    object.type_code()?;
    if object.as_stream_dict().is_some() {
        if raw_stream_data || filtered_stream_data {
            let warning_count = pdf.repair_diagnostics().entries().len();
            let data_result = if filtered_stream_data {
                if !object.stream_data_filterable(DecodeLevel::All)? {
                    object.warn_if_possible("unable to filter stream data")?;
                    return Err(Error::System(format!(
                        "unable to get object {}",
                        show_object_generation(object)
                    )));
                }
                if normalize_content {
                    let mut output = crate::pipeline::buffer::Buffer::new("stream data", None);
                    let mut _filtering_attempted = false;
                    let _stream_data_succeeded = object.pipe_stream_data(
                        &mut output,
                        &mut _filtering_attempted,
                        crate::object_handle::STREAM_ENCODE_NORMALIZE,
                        DecodeLevel::All,
                        false,
                        false,
                    )?; // cov:ignore: multiline call terminator has no executable coverage region
                    Ok(std::rc::Rc::new(output.take_buffer()?))
                } else {
                    object.get_stream_data(DecodeLevel::All)
                }
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
            write_to_standard_output(logger, data.as_ref())
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
}

fn show_object_generation(object: &ObjectHandle) -> String {
    object
        .object_ref()
        .map(|object_ref| format!("{},{}", object_ref.number, object_ref.generation))
        .unwrap_or_else(|| "0,0".to_owned())
}

fn emit_npages<R: Read + Seek>(pdf: &mut Pdf<R>, logger: &crate::QPDFLogger) -> Result<()> {
    let root = pdf.root_handle()?;
    let pages = root.try_get_key(b"/Pages")?;
    let count = pages.try_get_key(b"/Count")?.try_get_int_value()?;
    logger.info(format!("{count}\n"))
}

fn write_to_standard_output(logger: &crate::QPDFLogger, data: &[u8]) -> Result<()> {
    logger.save_to_standard_output(true)?;
    logger.get_save()?.write(data).map_err(Error::from)
}

fn emit_show_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    logger: &crate::QPDFLogger,
    show_page_images: bool,
) -> Result<()> {
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    for (index, page_handle) in pages.into_iter().enumerate() {
        let page_reference = String::from_utf8_lossy(&page_handle.unparse()).into_owned();
        logger.info(format!("page {}: {}\n", index + 1, page_reference))?;

        if show_page_images {
            let images =
                PageObjectHelper::from_object_handle(page_handle.clone(), pdf).get_images()?;
            if !images.is_empty() {
                logger.info("  images:\n")?;
                for (name, image) in images {
                    // `get_images` selects only `/Subtype /Image` stream XObjects, so
                    // this defensive error is unreachable through the canonical helper.
                    // cov:ignore-start: PageObjectHelper::get_images guarantees stream dictionaries for image XObjects
                    let dictionary = image.as_stream_dict().ok_or_else(|| {
                        Error::Internal("image XObject has no stream dictionary".to_owned())
                    })?;
                    // cov:ignore-end
                    let width = dictionary
                        .try_get_key(b"/Width")?
                        .try_get_int_value_as_int()?;
                    let height = dictionary
                        .try_get_key(b"/Height")?
                        .try_get_int_value_as_int()?;
                    let mut line = b"    ".to_vec();
                    line.extend_from_slice(&name);
                    line.extend_from_slice(b": ");
                    line.extend_from_slice(&image.unparse());
                    line.extend_from_slice(b", ");
                    line.extend_from_slice(width.to_string().as_bytes());
                    line.extend_from_slice(b" x ");
                    line.extend_from_slice(height.to_string().as_bytes());
                    line.push(b'\n');
                    logger.info(line)?;
                }
            }
        }

        // qpdf writes the section heading before asking the page helper for
        // its stream array (`QPDFJob.cc:869-872`). This preserves warning
        // order when a malformed `/Contents` value is encountered.
        logger.info("  content:\n")?;
        let contents =
            PageObjectHelper::from_object_handle(page_handle, pdf).get_page_contents()?;
        for content in contents {
            let mut line = b"    ".to_vec();
            line.extend_from_slice(&content.unparse());
            line.push(b'\n');
            logger.info(line)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectRef;

    fn quiet_job() -> QPDFJob {
        let logger = crate::QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
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

    #[test]
    fn show_object_reports_unfilterable_stream_before_object_error() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/test_driver/stream_unfilterable.pdf")
                .to_vec(),
        )
        .unwrap();
        let stream = pdf.get_object_handle(ObjectRef::new(6, 0));
        let error = quiet_job()
            .show_object(&mut pdf, stream, false, true)
            .expect_err("an unfilterable filter must remain an operation error");
        assert!(matches!(
            error,
            Error::System(message) if message == "unable to get object 6,0"
        ));
        assert!(pdf.repair_diagnostics().entries().iter().any(|warning| {
            warning.get_message_detail() == b"unable to filter stream data"
                && warning
                    .get_object()
                    .windows(b"stream object 6 0".len())
                    .any(|window| window == b"stream object 6 0")
        }));
    }

    #[test]
    fn show_xref_rejects_a_type_zero_entry_like_qpdf() {
        let mut pdf = recovered_pdf();
        pdf.resolver
            .insert_default_xref_entry_for_test(ObjectRef::new(99, 0));

        let error = quiet_job()
            .show_xref(&mut pdf)
            .expect_err("qpdf rejects a type-zero entry while showing xref");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "unknown cross-reference table type while showing xref_table"
        ));
    }

    struct RecordingInfoSink {
        bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl crate::pipeline::Pipeline for RecordingInfoSink {
        // cov:ignore-start: the logger never queries an info sink's identifier
        fn identifier(&self) -> &str {
            "recording info sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            self.bytes.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: the logger does not finish an info sink during show_xref
        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    #[test]
    fn show_xref_writes_the_partial_entry_prefix_before_rejecting_a_type_zero_entry() {
        // qpdf's `showXRefTable` writes `og.unparse('/') << ": "`
        // unconditionally before its type switch, so a `default` arm
        // (`std::logic_error`) still leaves that partial prefix on stdout
        // (`QPDF.cc:1219-1233`). Reproduces the `issue-143.pdf` divergence:
        // qpdf prints `0/0: ` with no trailing newline before erroring;
        // flpdf used to print nothing at all.
        let mut pdf = recovered_pdf();
        pdf.resolver
            .insert_default_xref_entry_for_test(ObjectRef::new(99, 0));

        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = crate::QPDFLogger::create();
        logger.set_info(Some(crate::pipeline::PipelineHandle::new(
            RecordingInfoSink {
                bytes: std::sync::Arc::clone(&bytes),
            },
        )));
        logger.set_warn(Some(logger.discard()));
        let mut job = QPDFJob::new();
        job.set_logger(logger);

        let error = job
            .show_xref(&mut pdf)
            .expect_err("qpdf rejects a type-zero entry while showing xref");
        assert!(matches!(
            error,
            Error::Internal(message)
                if message == "unknown cross-reference table type while showing xref_table"
        ));
        // The offending entry's prefix must reach stdout with no trailing
        // newline before the error, like qpdf.
        assert!(bytes.lock().unwrap().ends_with(b"99/0: "));
    }
}
