//! The public qpdf-shaped PDF writer lifecycle.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::encrypt_setup::{CopyEncryptionSource, EncryptParams};
use crate::pipeline::Pipeline;
use crate::{Error, ObjectRef, Pdf, Result, XrefEntry};

use super::settings::{DecodeLevel, WriterSettings};
use super::{
    effective_pdf_version, emit_canonical_pdf, report_progress, ObjectStreamMode, ProgressReporter,
    StreamDataMode, WriterOptions, WriterResult,
};
use crate::linearization::writer::write_linearized_for_pdf_writer;

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
/// This lifecycle owns the complete canonical full-rewrite pipeline and its
/// qpdf-compatible settings.
pub struct PdfWriter<'pdf, R: Read + Seek + 'static> {
    pdf: &'pdf mut Pdf<R>,
    settings: WriterSettings,
    output: Option<WriterOutput>,
    write_started: bool,
    write_succeeded: bool,
    result: Option<WriterResult>,
}

impl<'pdf, R: Read + Seek + 'static> PdfWriter<'pdf, R> {
    /// Create a writer around a live PDF document.
    pub fn new(pdf: &'pdf mut Pdf<R>) -> Self {
        Self {
            pdf,
            settings: WriterSettings::default(),
            output: None,
            write_started: false,
            write_succeeded: false,
            result: None,
        }
    }

    fn ensure_output_unconfigured(&self) -> Result<()> {
        if self.output.is_some() || self.write_started {
            return Err(Error::Unsupported(
                "PdfWriter output can be configured only once".into(),
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
        // PdfWriter's stream-data setters are state transitions, not a
        // late override layered on top of setDecodeLevel/setCompressStreams.
        // QPDFWriter.cc raises the decode floor for uncompress/compress,
        // clears it for preserve, and toggles compression at the same time.
        // Keep the translated state in the ordinary settings fields so setter
        // order has the same observable result as qpdf.
        self.settings.stream_data_mode = None;
        match mode {
            StreamDataMode::Preserve => {
                self.settings.decode_level = DecodeLevel::None;
                self.settings.compress_streams = false;
            }
            StreamDataMode::Uncompress => {
                self.settings.decode_level =
                    self.settings.decode_level.max(DecodeLevel::Generalized);
                self.settings.compress_streams = false;
            }
            StreamDataMode::Compress => {
                self.settings.decode_level =
                    self.settings.decode_level.max(DecodeLevel::Generalized);
                self.settings.compress_streams = true;
            }
        }
        self.settings.decode_level_set = true;
        self.settings.compress_streams_set = true;
    }

    pub fn set_compress_streams(&mut self, value: bool) {
        // qpdf's setCompressStreams changes only this flag. In
        // particular, it must not raise the initial decode level from none.
        self.settings.compress_streams = value;
        self.settings.compress_streams_set = true;
    }

    pub fn set_decode_level(&mut self, level: DecodeLevel) {
        self.settings.decode_level = level;
        self.settings.decode_level_set = true;
    }

    pub fn set_recompress_flate(&mut self, value: bool) {
        self.settings.recompress_flate = value;
    }

    pub fn set_content_normalization(&mut self, value: bool) {
        self.settings.content_normalization = value;
        self.settings.content_normalization_set = true;
    }

    pub fn set_qdf_mode(&mut self, value: bool) {
        self.settings.qdf_mode = value;
    }

    pub fn set_preserve_unreferenced_objects(&mut self, value: bool) {
        self.settings.preserve_unreferenced_objects = value;
    }

    pub fn set_newline_before_endstream(&mut self, value: bool) {
        self.settings.newline_before_endstream = if value {
            super::NewlineBeforeEndstream::Yes
        } else {
            super::NewlineBeforeEndstream::Never
        };
    }

    /// Set qpdf's three-state endstream framing policy used by the CLI and
    /// byte-parity tests. `Never` is qpdf's default; `No` adds a newline only
    /// when the payload does not already end in one.
    pub fn set_newline_before_endstream_mode(&mut self, value: super::NewlineBeforeEndstream) {
        self.settings.newline_before_endstream = value;
    }

    pub fn set_minimum_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) {
        let version = version.into();
        match self.settings.minimum_pdf_version.as_mut() {
            None => self.settings.minimum_pdf_version = Some((version, extension_level)),
            Some((current_version, current_extension_level)) => {
                let current = crate::pdf_version::parse_pdf_version(current_version)
                    .expect("validated minimum PDF version remains parseable");
                let candidate = crate::pdf_version::parse_pdf_version(&version)
                    .expect("validated minimum PDF version is parseable");
                if candidate > current
                    || (candidate == current && extension_level > *current_extension_level)
                {
                    *current_version = version;
                    *current_extension_level = extension_level;
                }
            }
        }
    }

    pub fn force_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) {
        let version = version.into();
        self.settings.forced_pdf_version = Some((version, extension_level));
    }

    pub fn set_extra_header_text(&mut self, text: impl Into<String>) {
        let mut text = text.into();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        self.settings.extra_header_text = text;
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
        if value {
            self.settings.pclm = false;
        }
    }

    pub fn set_linearization_pass1_filename(&mut self, path: impl Into<PathBuf>) {
        self.settings.linearization_pass1_filename = Some(path.into());
    }

    pub fn set_pclm(&mut self, value: bool) {
        self.settings.pclm = value;
        if value {
            self.settings.linearization = false;
        }
    }

    pub fn register_progress_reporter(&mut self, reporter: Box<dyn FnMut(u8) + 'static>) {
        self.settings.progress_reporter = Some(ProgressReporter::new(reporter));
    }

    /// Return the effective header version before writing.
    pub fn get_final_version(&mut self) -> Result<String> {
        let options = self.prepared_write_options()?;
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
    /// Validation errors occur before `write_started` is consumed and may be
    /// corrected and retried. Once emission begins, an emission or sink failure
    /// is permanently one-shot and cannot be retried.
    pub fn write(&mut self) -> Result<()> {
        if self.write_started {
            return Err(Error::Unsupported(
                "PdfWriter::write may be called only once".into(),
            ));
        }
        if self.output.is_none() {
            return Err(Error::Unsupported(
                "PdfWriter::write requires an output sink".into(),
            ));
        }
        self.validate_supported_settings()?;
        let mut options = self.prepared_write_options()?;
        self.write_started = true;
        report_progress(&options, 0);
        let (bytes, result) = if self.settings.linearization {
            options.qdf = false;
            let pass1_path = self.settings.linearization_pass1_filename.as_deref();
            let (mut document, result) =
                write_linearized_for_pdf_writer(self.pdf, &options, pass1_path)?;
            document.back_patch()?;
            (document.bytes, result)
        } else {
            let mut bytes = Vec::new();
            let result = emit_canonical_pdf(self.pdf, &mut bytes, &options)?;
            (bytes, result)
        };

        self.output
            .as_mut()
            .expect("output was checked before writing")
            .write_complete(bytes)?;
        report_progress(&options, 100);
        self.result = Some(result);
        self.write_succeeded = true;
        Ok(())
    }

    /// Return the output identity actually assigned to a source object.
    pub fn get_renumbered_obj_gen(&self, source: ObjectRef) -> Result<Option<ObjectRef>> {
        self.ensure_write_succeeded()?;
        Ok(self
            .result
            .as_ref()
            .expect("successful writes retain their result")
            .old_to_new
            .get(&source)
            .copied())
    }

    /// Return the xref records actually written by the completed emitter.
    pub fn get_written_xref_table(&self) -> Result<BTreeMap<ObjectRef, XrefEntry>> {
        self.ensure_write_succeeded()?;
        Ok(self
            .result
            .as_ref()
            .expect("successful writes retain their result")
            .written_xref
            .clone())
    }

    /// Validate the configured qpdf writer state before consuming the output
    /// lifecycle. Setter combinations that qpdf resolves by precedence are
    /// normalized during the writer's private preparation phase.
    pub fn validate_supported_settings(&self) -> Result<()> {
        Ok(())
    }

    /// Translate the qpdf-shaped settings into the one immutable option set
    /// consumed by the full-rewrite emitter.
    ///
    /// qpdf performs this part of setup after all public setters have run:
    /// explicit encryption/copy parameters win over preservation; qdf,
    /// content normalization, non-none decoding, and PCLm disable source
    /// preservation; and a forced header version can disable an otherwise
    /// valid encryption scheme. Keeping the preparation in one method makes
    /// `get_final_version` and `write` observe the same plan.
    fn prepared_write_options(&mut self) -> Result<WriterOptions> {
        let mut options = self.settings.to_write_options();
        if self.settings.linearization {
            // qpdf's doWriteSetup clears QDF before selecting the
            // linearized two-pass writer (QPDFWriter.cc:2036-2038).
            options.qdf = false;
        }
        if options.pclm {
            // qpdf's doWriteSetup makes PCLm a cleartext, unfiltered
            // output mode before source-encryption preservation is considered.
            options.encrypt = None;
            options.copy_encryption = None;
            options.qdf = false;
            options.content_normalization = false;
            options.decode_level = DecodeLevel::None;
            options.compress_streams = crate::CompressStreams::No;
            options.stream_data = None;
            options.object_streams = ObjectStreamMode::Disable;
        }
        let can_preserve = self.settings.preserve_encryption
            && self.pdf.is_encrypted()
            && options.encrypt.is_none()
            && options.copy_encryption.is_none()
            && !options.qdf
            && !options.content_normalization
            && options.decode_level == DecodeLevel::None
            && !self.settings.pclm;
        if can_preserve {
            options.copy_encryption = self.pdf.writer_copy_encryption_source()?;
        }

        if forced_version_disables_encryption(&options) {
            options.encrypt = None;
            options.copy_encryption = None;
        }
        Ok(options)
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

/// qpdf's `disableIncompatibleEncryption` for the writer options
/// that have reached this bridge. A valid forced version is a hard cap: when
/// it cannot represent the selected Standard security handler, qpdf silently
/// drops encryption and writes the rewritten objects in cleartext.
fn forced_version_disables_encryption(options: &WriterOptions) -> bool {
    let Some(forced) = options
        .force_version
        .as_deref()
        .and_then(crate::pdf_version::parse_pdf_version)
    else {
        return false;
    };

    let Some((version, revision, use_aes)) = encryption_shape(options) else {
        return false;
    };

    let v = crate::pdf_version::PdfVersion::new(1, 3, 0);
    if forced < v {
        return true;
    }
    let v = crate::pdf_version::PdfVersion::new(1, 4, 0);
    if forced < v && (version > 1 || revision > 2) {
        return true;
    }
    let v = crate::pdf_version::PdfVersion::new(1, 5, 0);
    if forced < v && (version > 2 || revision > 3) {
        return true;
    }
    let v = crate::pdf_version::PdfVersion::new(1, 6, 0);
    if forced < v && use_aes {
        return true;
    }
    let v = crate::pdf_version::PdfVersion::new(1, 7, 0);
    (forced < v || (forced == v && options.force_extension_level.unwrap_or(0) < 3))
        && (version >= 5 || revision >= 5)
}

fn encryption_shape(options: &WriterOptions) -> Option<(i64, i64, bool)> {
    if let Some(params) = options.encrypt.as_ref() {
        use crate::encrypt_setup::EncryptMethod;
        return Some(match params.method {
            EncryptMethod::V1Rc440 => (1, 2, false),
            EncryptMethod::V2Rc4128 => (2, 3, false),
            EncryptMethod::V4Rc4128 => (4, 4, false),
            EncryptMethod::V4Aes128 => (4, 4, true),
            EncryptMethod::V5R5Aes256 => (5, 5, true),
            EncryptMethod::V5R6Aes256 => (5, 6, true),
        });
    }

    let source = options.copy_encryption.as_ref()?;
    let version = source.encrypt_dict.get("V")?.as_integer()?;
    let revision = source.encrypt_dict.get("R")?.as_integer()?;
    Some((version, revision, version >= 4))
}
