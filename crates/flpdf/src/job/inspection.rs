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
            emit_show_object(pdf, &logger, &object, raw_stream_data, filtered_stream_data)
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
        emit_show_object(pdf, &logger, object, raw_stream_data, filtered_stream_data)
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
                // Same reclassification concern as ensure_present above: a
                // present-but-non-stream object is malformed selection, not
                // an unsupported feature, and this diagnostic predates the
                // migration -- keep it bare via Error::System.
                return Err(Error::System(format!(
                    "object {} {} R is not a stream",
                    object_ref.number, object_ref.generation
                )));
            };

            if raw_stream_data {
                let raw = object.get_raw_stream_data()?;
                return write_to_standard_output(&logger, raw.as_ref());
            }

            // Preserve the existing CLI marker for the specialized codecs that
            // qpdf keeps as raw data in this command. The name is read from the
            // live stream dictionary; no filter dictionary is materialized.
            if let Some(filter_name) = first_stream_filter_name(&stream_dictionary)? {
                if !crate::filters::is_decoded_filter(&filter_name) {
                    if let Some(label) = crate::filters::passthrough_codec_label(&filter_name) {
                        let raw = object.get_raw_stream_data()?;
                        let len = raw.len();
                        return logger.info(format!("<binary, {len} bytes, codec {label}>\n"));
                    }
                }
            }

            let decoded = object.get_stream_data(DecodeLevel::All)?;
            write_to_standard_output(&logger, decoded.as_ref())
        })
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

    /// Show the effective cross-reference table through qpdf's inspection
    /// lifecycle (`QPDF::showXRefTable`, `libqpdf/QPDF.cc:1213-1240`). The
    /// reader-owned snapshot is the same table qpdf exposes through
    /// `QPDF::getXRefTable` (`QPDF.cc:2370-2377`), so this consumer does not
    /// inspect raw xref-stream bytes or reconstruct a second table.
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
    for (object_ref, entry) in pdf.get_xref_table() {
        let line = match entry {
            XrefEntry::Free { .. } => {
                return Err(Error::Internal(
                    "unknown cross-reference table type while showing xref_table".to_owned(),
                ));
            }
            XrefEntry::Uncompressed { offset } => format!(
                "{}/{}: uncompressed; offset = {offset}\n",
                object_ref.number, object_ref.generation
            ),
            XrefEntry::Compressed { stream, index } => format!(
                "{}/{}: compressed; stream = {stream}, index = {index}\n",
                object_ref.number, object_ref.generation
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
) -> Result<()> {
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
    object_ref: ObjectRef,
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
        let recovered_eol = pdf.canonical_recovered_stream_eol(object_ref, object)?;
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
        assert!(error
            .to_string()
            .contains("getStreamData called on unfilterable stream"));
    }

    #[test]
    fn show_stream_returns_an_unknown_filter_error_after_marker_probe() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let stream_ref = ObjectRef::new(7, 0);
        let stream = pdf.get_object_handle(stream_ref);
        pdf.resolve(&stream).unwrap();
        stream
            .as_stream_dict()
            .unwrap()
            .replace_key(b"/Filter", ObjectHandle::name(b"UnknownFilter".to_vec()))
            .unwrap();
        let error = quiet_job()
            .show_stream(&mut pdf, stream_ref, false)
            .expect_err("an unknown filter must remain an operation error");
        assert!(error
            .to_string()
            .contains("getStreamData called on unfilterable stream"));
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
}
