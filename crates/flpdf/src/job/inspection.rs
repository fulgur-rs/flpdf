//! qpdf correspondence: `QPDFJob::doShowObj`, `doShowPages`, and object/stream inspection helpers (`libqpdf/QPDFJob.cc:805-874`).
//!
//! The CLI owns argument parsing, while this module owns the live
//! `ObjectHandle` inspection and the shared job completion boundary. Keeping
//! this code here prevents an external binary crate from falling back to the
//! legacy `Object` materialization route for read-only inspection.

use super::lifecycle::{JobExitCode, QPDFJob};
use crate::writer::DecodeLevel;
use crate::{
    Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf, Result, XrefEntry,
};
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
            let mut output = unparse_object_with_stream_data(pdf, &object)?;
            output.push(b'\n');
            logger.info(output)
        })
    }

    /// Dump one object selected by qpdf's raw signed object/generation pair.
    /// This is the inspection counterpart for headers whose generation cannot
    /// cross the public `ObjectRef` projection boundary.
    pub fn dump_object_by_raw_identity<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        object_number: i32,
        generation: i32,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| {
            let object = pdf.get_object_handle_by_raw_identity(object_number, generation);
            object.type_code()?;
            // A stream selected by its raw identity has to serialize like any
            // other: `unparse_resolved` alone would emit only the indirect
            // reference, dropping the dictionary, framing and bytes this
            // command exists to print. The helper uses the handle's raw qpdf
            // identity and parsed stream-data offset directly, so no
            // `ObjectRef` projection is needed for an out-of-range generation.
            let mut output = unparse_object_with_stream_data(pdf, &object)?;
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
        let line = match entry {
            XrefEntry::Free { .. } => {
                return Err(Error::Internal(
                    "unknown cross-reference table type while showing xref_table".to_owned(),
                ));
            }
            XrefEntry::Uncompressed { offset } => format!(
                "{}/{}: uncompressed; offset = {offset}\n",
                object_ref.get_obj(),
                object_ref.get_gen()
            ),
            XrefEntry::Compressed { stream, index } => format!(
                "{}/{}: compressed; stream = {stream}, index = {index}\n",
                object_ref.get_obj(),
                object_ref.get_gen()
            ),
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
) -> Result<Vec<u8>> {
    if let Some(dictionary) = object.as_stream_dict() {
        let data = object.get_raw_stream_data()?;
        let mut output = dictionary.unparse_resolved();
        output.extend_from_slice(b"\nstream\n");
        let data = data.as_ref();
        // qpdf-deviation-start: `dump-object` has no qpdf counterpart
        // (QPDFJob::doShowObj prints "Object is stream.  Dictionary:" and never
        // reserializes stream framing, QPDFJob.cc:806-832). This flpdf-only
        // reserializer drops the recovered-length EOL so its own
        // "\nendstream" framing does not double the source line ending.
        let recovered_eol = pdf.canonical_recovered_stream_eol(object)?;
        let data = if let Some(eol) = recovered_eol.filter(|eol| data.ends_with(eol)) {
            &data[..data.len() - eol.len()]
        } else {
            data
        };
        // qpdf-deviation-end
        output.extend_from_slice(data);
        output.extend_from_slice(b"\nendstream");
        Ok(output)
    } else {
        Ok(object.unparse_resolved())
    }
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
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for (index, page_ref) in page_refs.iter().enumerate() {
        logger.info(format!("page {}: {}\n", index + 1, page_ref))?;

        if show_page_images {
            let images = PageObjectHelper::new(*page_ref, pdf).get_images()?;
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
        let contents = PageObjectHelper::new(*page_ref, pdf).get_page_contents()?;
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
    fn unparse_stream_keeps_source_bytes_without_recovered_framing() {
        let pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let output = unparse_object_with_stream_data(&pdf, &stream()).unwrap();
        assert!(output.ends_with(b"encoded\nendstream"));
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

    fn xref_stream_entry(kind: u8, value: u32, generation: u16) -> [u8; 7] {
        let value = value.to_be_bytes();
        let generation = generation.to_be_bytes();
        [
            kind,
            value[0],
            value[1],
            value[2],
            value[3],
            generation[0],
            generation[1],
        ]
    }

    fn append_reused_xref_stream(
        bytes: &mut Vec<u8>,
        object_offsets: (u32, u32),
        length: usize,
        previous: Option<usize>,
    ) -> usize {
        let xref_offset = bytes.len();
        let mut dictionary = format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 4 2] /Index [5 1 0 5] /Length {length}"
        );
        if let Some(previous) = previous {
            dictionary.push_str(&format!(" /Prev {previous}"));
        }
        dictionary.push_str(" >>\nstream\n");
        bytes.extend_from_slice(dictionary.as_bytes());

        let entries = [
            xref_stream_entry(1, xref_offset as u32, 0),
            xref_stream_entry(0, 0, u16::MAX),
            xref_stream_entry(1, object_offsets.0, 0),
            xref_stream_entry(1, object_offsets.1, 0),
            xref_stream_entry(0, 0, 0),
            // A free-entry generation is ignored by qpdf; retaining 0x0a as
            // its final byte gives the new stream a legitimate trailing LF.
            xref_stream_entry(0, 0, 10),
        ];
        assert_eq!(entries.len() * 7, 42);
        assert_eq!(entries.last().unwrap().last(), Some(&b'\n'));
        for entry in entries {
            bytes.extend_from_slice(&entry);
        }
        bytes.extend_from_slice(b"endstream\nendobj\n");
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        xref_offset
    }

    #[test]
    fn dump_object_keeps_new_revision_payload_when_xref_object_is_reused() {
        let mut bytes = b"%PDF-1.5\n".to_vec();
        let object_one_offset = bytes.len() as u32;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let object_two_offset = bytes.len() as u32;
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
        let base_xref =
            append_reused_xref_stream(&mut bytes, (object_one_offset, object_two_offset), 1, None);
        append_reused_xref_stream(
            &mut bytes,
            (object_one_offset, object_two_offset),
            42,
            Some(base_xref),
        );

        let mut pdf = Pdf::open_mem_owned(bytes).expect("reused xref fixture should open");
        assert!(
            !pdf.reconstructed_xref(),
            "the xref chain must stay canonical"
        );
        let object = pdf.get_object_handle(ObjectRef::new(5, 0));
        object
            .try_is_scalar()
            .expect("new xref stream should resolve");
        let output =
            unparse_object_with_stream_data(&pdf, &object).expect("dump-object serialization");
        let stream_start = output
            .windows(b"\nstream\n".len())
            .position(|window| window == b"\nstream\n")
            .map(|position| position + b"\nstream\n".len())
            .expect("stream framing");
        let stream_end = output[stream_start..]
            .windows(b"\nendstream".len())
            .position(|window| window == b"\nendstream")
            .map(|position| stream_start + position)
            .expect("endstream framing");
        assert_eq!(
            output[stream_start..stream_end].len(),
            42,
            "the current xref stream must retain its complete raw payload"
        );
        assert_eq!(
            output[stream_end - 1],
            b'\n',
            "the current payload's legitimate trailing LF must survive"
        );
    }
}
