//! The public qpdf-shaped PDF writer lifecycle.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::encrypt_setup::{CopyEncryptionSource, EncryptParams};
use crate::pipeline::Pipeline;
use crate::{Error, ObjectRef, Pdf, Result, XrefEntry};

use super::settings::{DecodeLevel, WriterSettings};
use super::{effective_pdf_version, write_pdf_full_rewrite, ObjectStreamMode, StreamDataMode};

enum WriterOutput {
    Memory(Option<Vec<u8>>),
    Writer(Box<dyn Write>),
    Pipeline(Box<dyn Pipeline>),
}

impl WriterOutput {
    fn write_complete(&mut self, bytes: Vec<u8>) -> Result<()> {
        match self {
            Self::Memory(buffer) => {
                *buffer = Some(bytes);
                Ok(())
            }
            Self::Writer(writer) => {
                writer.write_all(&bytes)?;
                writer.flush()?;
                Ok(())
            }
            Self::Pipeline(pipeline) => {
                pipeline.write(&bytes)?;
                pipeline.finish()?;
                Ok(())
            }
        }
    }

    fn take_memory(&mut self) -> Result<Vec<u8>> {
        match self {
            Self::Memory(buffer) => buffer.take().ok_or_else(|| {
                Error::Unsupported("get_buffer is only available once after a memory write".into())
            }),
            Self::Writer(_) | Self::Pipeline(_) => Err(Error::Unsupported(
                "get_buffer requires a successful memory output".into(),
            )),
        }
    }
}

/// A qpdf-shaped writer for producing one fresh PDF output.
///
/// This first slice exposes the lifecycle and settings surface while using
/// flpdf's existing private full-rewrite emitter internally. The legacy free
/// functions remain available temporarily for consumer migration.
pub struct QPDFWriter<'pdf, R: Read + Seek + 'static> {
    pdf: &'pdf mut Pdf<R>,
    settings: WriterSettings,
    output: Option<WriterOutput>,
    write_started: bool,
    write_succeeded: bool,
}

impl<'pdf, R: Read + Seek + 'static> QPDFWriter<'pdf, R> {
    /// Create a writer around a live PDF document.
    pub fn new(pdf: &'pdf mut Pdf<R>) -> Self {
        Self {
            pdf,
            settings: WriterSettings::default(),
            output: None,
            write_started: false,
            write_succeeded: false,
        }
    }

    fn ensure_output_unconfigured(&self) -> Result<()> {
        if self.output.is_some() || self.write_started {
            return Err(Error::Unsupported(
                "QPDFWriter output can be configured only once".into(),
            ));
        }
        Ok(())
    }

    /// Configure qpdf-style `wb+` file output.
    pub fn set_output_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.ensure_output_unconfigured()?;
        let path = path.as_ref();
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|error| Error::file_io("open output", path.to_path_buf(), error))?;
        self.output = Some(WriterOutput::Writer(Box::new(file)));
        Ok(())
    }

    /// Configure an owned arbitrary writer sink.
    pub fn set_output_writer<W: Write + 'static>(&mut self, writer: W) -> Result<()> {
        self.ensure_output_unconfigured()?;
        self.output = Some(WriterOutput::Writer(Box::new(writer)));
        Ok(())
    }

    /// Configure an owned in-memory output sink.
    pub fn set_output_memory(&mut self) -> Result<()> {
        self.ensure_output_unconfigured()?;
        self.output = Some(WriterOutput::Memory(None));
        Ok(())
    }

    /// Configure an owned Pipeline sink.
    pub fn set_output_pipeline<P: Pipeline + 'static>(&mut self, pipeline: P) -> Result<()> {
        self.ensure_output_unconfigured()?;
        self.output = Some(WriterOutput::Pipeline(Box::new(pipeline)));
        Ok(())
    }

    /// Take the memory output exactly once after a successful write.
    pub fn get_buffer(&mut self) -> Result<Vec<u8>> {
        if !self.write_succeeded {
            return Err(Error::Unsupported(
                "get_buffer requires a successful write".into(),
            ));
        }
        self.output
            .as_mut()
            .ok_or_else(|| Error::Unsupported("get_buffer requires a memory output".into()))?
            .take_memory()
    }

    pub fn set_object_stream_mode(&mut self, mode: ObjectStreamMode) {
        self.settings.object_stream_mode = mode;
    }

    pub fn set_stream_data_mode(&mut self, mode: StreamDataMode) {
        self.settings.stream_data_mode = Some(mode);
    }

    pub fn set_compress_streams(&mut self, value: bool) {
        self.settings.compress_streams = value;
    }

    pub fn set_decode_level(&mut self, level: DecodeLevel) {
        self.settings.decode_level = level;
    }

    pub fn set_recompress_flate(&mut self, value: bool) {
        self.settings.recompress_flate = value;
    }

    pub fn set_content_normalization(&mut self, value: bool) {
        self.settings.content_normalization = value;
    }

    pub fn set_qdf_mode(&mut self, value: bool) {
        self.settings.qdf_mode = value;
    }

    pub fn set_preserve_unreferenced_objects(&mut self, value: bool) {
        self.settings.preserve_unreferenced_objects = value;
    }

    pub fn set_newline_before_endstream(&mut self, value: bool) {
        self.settings.newline_before_endstream = value;
    }

    pub fn set_minimum_pdf_version(
        &mut self,
        version: impl Into<String>,
        extension_level: i64,
    ) -> Result<()> {
        let version = version.into();
        validate_pdf_version(&version)?;
        self.settings.minimum_pdf_version = Some((version, extension_level));
        Ok(())
    }

    pub fn force_pdf_version(
        &mut self,
        version: impl Into<String>,
        extension_level: i64,
    ) -> Result<()> {
        let version = version.into();
        validate_pdf_version(&version)?;
        self.settings.forced_pdf_version = Some((version, extension_level));
        Ok(())
    }

    pub fn set_extra_header_text(&mut self, text: impl Into<String>) {
        self.settings.extra_header_text = text.into();
    }

    pub fn set_deterministic_id(&mut self, value: bool) {
        self.settings.deterministic_id = value;
    }

    pub fn set_static_id(&mut self, value: bool) {
        self.settings.static_id = value;
    }

    pub fn set_static_aes_iv(&mut self, value: bool) {
        self.settings.static_aes_iv = value;
    }

    pub fn set_suppress_original_object_ids(&mut self, value: bool) {
        self.settings.suppress_original_object_ids = value;
    }

    pub fn set_preserve_encryption(&mut self, value: bool) {
        self.settings.preserve_encryption = value;
    }

    pub fn set_encryption_parameters(&mut self, params: EncryptParams) {
        self.settings.encryption_parameters = Some(params);
        self.settings.copy_encryption = None;
    }

    pub fn copy_encryption_parameters(&mut self, source: CopyEncryptionSource) {
        self.settings.copy_encryption = Some(source);
        self.settings.encryption_parameters = None;
    }

    pub fn set_linearization(&mut self, value: bool) {
        self.settings.linearization = value;
    }

    pub fn set_linearization_pass1_filename(&mut self, path: impl Into<PathBuf>) {
        self.settings.linearization_pass1_filename = Some(path.into());
    }

    pub fn set_pclm(&mut self, value: bool) {
        self.settings.pclm = value;
    }

    pub fn register_progress_reporter(&mut self, reporter: Box<dyn FnMut(u8) + 'static>) {
        self.settings.progress_reporter = Some(reporter);
    }

    /// Return the effective header version before writing.
    pub fn get_final_version(&mut self) -> Result<String> {
        let options = self.settings.to_write_options();
        // The exact qpdf output-plan floor for preserve mode is a later task.
        Ok(effective_pdf_version(
            self.pdf.version(),
            &options,
            self.settings.linearization,
            matches!(self.settings.object_stream_mode, ObjectStreamMode::Generate),
        )
        .to_owned())
    }

    /// Write one fresh PDF output and finish the configured sink once.
    ///
    /// `write_started` is consumed before emission; any failure is permanently
    /// one-shot and cannot be retried.
    pub fn write(&mut self) -> Result<()> {
        if self.write_started {
            return Err(Error::Unsupported(
                "QPDFWriter::write may be called only once".into(),
            ));
        }
        if self.output.is_none() {
            return Err(Error::Unsupported(
                "QPDFWriter::write requires an output sink".into(),
            ));
        }
        self.validate_supported_settings()?;
        self.write_started = true;

        let options = self.settings.to_write_options();
        let mut bytes = Vec::new();
        write_pdf_full_rewrite(self.pdf, &mut bytes, &options)?;

        self.output
            .as_mut()
            .expect("output was checked before writing")
            .write_complete(bytes)?;
        self.write_succeeded = true;
        Ok(())
    }

    /// Query the temporary result surface after a successful write.
    pub fn get_renumbered_obj_gen(&self, _source: ObjectRef) -> Result<Option<ObjectRef>> {
        self.ensure_write_succeeded()?;
        Err(Error::Unsupported(
            "renumbered object generation results are temporarily not implemented".into(),
        ))
    }

    /// Query the temporary result surface after a successful write.
    pub fn get_written_xref_table(&self) -> Result<BTreeMap<ObjectRef, XrefEntry>> {
        self.ensure_write_succeeded()?;
        Err(Error::Unsupported(
            "written xref table results are temporarily not implemented".into(),
        ))
    }

    /// Reject qpdf settings that the temporary emitter bridge cannot honor.
    pub fn validate_supported_settings(&self) -> Result<()> {
        let unsupported = if self.settings.content_normalization {
            Some("content normalization")
        } else if self.settings.preserve_unreferenced_objects {
            Some("preserving unreferenced objects")
        } else if self.settings.decode_level != DecodeLevel::Generalized {
            Some("non-generalized decode levels")
        } else if !self.settings.extra_header_text.is_empty() {
            Some("extra header text")
        } else if self.settings.linearization
            || self.settings.linearization_pass1_filename.is_some()
        {
            Some("linearization")
        } else if self.settings.pclm {
            Some("PCLm output")
        } else if self.settings.progress_reporter.is_some() {
            Some("progress reporting")
        } else {
            None
        };

        if let Some(setting) = unsupported {
            return Err(Error::Unsupported(format!(
                "{setting} is temporarily not implemented by QPDFWriter"
            )));
        }

        if self.pdf.is_encrypted()
            && self.settings.preserve_encryption
            && self.settings.encryption_parameters.is_none()
            && self.settings.copy_encryption.is_none()
        {
            return Err(Error::Unsupported(
                "preserving encryption for encrypted input is temporarily not implemented by QPDFWriter".into(),
            ));
        }

        Ok(())
    }

    fn ensure_write_succeeded(&self) -> Result<()> {
        if !self.write_succeeded {
            return Err(Error::Unsupported(
                "writer result queries require a successful write".into(),
            ));
        }
        Ok(())
    }
}

fn validate_pdf_version(version: &str) -> Result<()> {
    if crate::pdf_version::parse_pdf_version(version).is_none() {
        return Err(Error::Unsupported(format!(
            "invalid PDF version: {version}"
        )));
    }
    Ok(())
}
