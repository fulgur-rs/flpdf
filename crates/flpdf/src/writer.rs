//! PDF writer orchestration and shared writer modules.
//!
//! qpdf correspondence: QPDFWriter.cc writer lifecycle and responsibilities shared with writer submodules and linearization.
//!
#[path = "writer/encrypted_strings.rs"]
pub(crate) mod encrypted_strings;
#[path = "writer/encryption_state.rs"]
pub(crate) mod encryption_state;
#[path = "writer/object.rs"]
pub(crate) mod object;
#[path = "writer/object_streams/mod.rs"]
pub(crate) mod object_streams;
#[path = "writer/output.rs"]
pub(crate) mod output;
#[path = "writer/pclm.rs"]
pub(crate) mod pclm;
#[path = "writer/plain/mod.rs"]
pub(crate) mod plain;
#[path = "writer/rewrite_renumber.rs"]
pub(crate) mod rewrite_renumber;
#[path = "writer/serialize.rs"]
pub(crate) mod serialize;
mod settings;
#[path = "writer/write_object.rs"]
pub(crate) mod write_object;
use crate::qpdf_obj_gen::QpdfObjGen;
pub(crate) use object::{ObjectWriterEmission, StreamDictionaryOptions};
pub use object_streams::ObjectStreamMode;
use output::{OutputSink, OutputTarget};
pub use serialize::write_stream_to_buf;
pub use settings::DecodeLevel;
use settings::WriterSettings;

/// Test-only convenience for exercising the canonical qpdf writer lifecycle
/// from crate-internal unit suites. This deliberately has no public alias for
/// the removed free writer routes.
#[cfg(test)]
pub(crate) fn write_qpdf_to_memory<R, F>(pdf: &mut Pdf<R>, configure: F) -> Result<Vec<u8>>
where
    R: Read + Seek + 'static,
    F: FnOnce(&mut PdfWriter<'_, R>),
{
    let mut writer = PdfWriter::new(pdf);
    configure(&mut writer);
    writer.set_output_memory()?;
    writer.write()?;
    writer.get_buffer()
}

use crate::encryption::{
    CopyEncryptionSource, EncryptMethod, EncryptParams, PasswordMode, PasswordWriteNotice,
};
use crate::linearization::writer::write_linearized_for_pdf_writer;
use crate::pdf_version::{parse_qpdf_writer_version, PdfVersion, QpdfVersionParts};
use crate::pipeline::{flate::Flate, Pipeline, PipelineError, PipelineResult};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result, XrefEntry};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// File sink for qpdf's `QPDFWriter::setOutputFilename` path.
///
/// qpdf opens named output files as `wb+` (`QPDFWriter.cc:83-98`) and routes
/// writes through a stdio-buffered `Pl_StdioFile`. Its final
/// `Pl_StdioFile::finish` ignores non-EBADF flush errors
/// (`Pl_StdioFile.cc:41-45`), and `QPDFWriter::write` also discards `fclose`'s
/// result (`QPDFWriter.cc:2187-2212`). Keep the 4096-byte boundary and the
/// final non-EBADF error swallowing for the owned file-output route while
/// leaving arbitrary caller-provided writers strict.
/// The pipeline name qpdf gives every output file sink
/// (`QPDFWriter.cc:101-110`).
const QPDF_OUTPUT_PIPELINE: &str = "qpdf output";

struct QpdfFileWriter {
    output: BufWriter<File>,
}

impl QpdfFileWriter {
    const BUFFER_SIZE: usize = 4096;

    fn new(file: File) -> Self {
        Self {
            output: BufWriter::with_capacity(Self::BUFFER_SIZE, file),
        }
    }
}

impl Write for QpdfFileWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.output.flush() {
            Err(error) if error.raw_os_error() == Some(9) => {
                // cov:ignore-start: the owned file cannot be externally closed while this writer is live
                Err(error)
                // cov:ignore-end
            }
            Ok(()) | Err(_) => Ok(()),
        }
    }
}

enum WriterOutput {
    Memory(Option<Vec<u8>>),
    /// A `Write` sink, optionally carrying the pipeline identifier qpdf gives
    /// its own file output.
    ///
    /// `QPDFWriter::setOutputFile` wraps the `FILE*` in
    /// `Pl_StdioFile("qpdf output", file)` (`QPDFWriter.cc:101-110`) for both
    /// a named file and standard output, so a failed write reports that
    /// pipeline rather than any filename (`Pl_StdioFile.cc:25-37`). Sinks with
    /// no qpdf counterpart carry `None` and keep the bare I/O error.
    Writer {
        identifier: Option<&'static str>,
        writer: Box<dyn Write>,
    },
    Pipeline(Box<dyn Pipeline>),
}

/// Build the failure qpdf's `Pl_StdioFile` raises for a sink write
/// (`Pl_StdioFile.cc:25-37`), or `None` for a sink qpdf does not name.
fn stdio_sink_failure(identifier: Option<&'static str>, source: &io::Error) -> Option<Error> {
    identifier.map(|identifier| {
        Error::SystemBytes(
            format!(
                "{identifier}: Pl_StdioFile::write: {}",
                crate::job::qpdf_file_io_source_message(source)
            )
            .into_bytes(),
        )
    })
}

struct WriterOutputSink<'a> {
    output: &'a mut WriterOutput,
    failure: Option<Error>,
}

impl<'a> WriterOutputSink<'a> {
    fn new(output: &'a mut WriterOutput) -> Self {
        Self {
            output,
            failure: None,
        }
    }

    fn finish_output(&mut self) -> Result<()> {
        self.flush()?;
        match self.output {
            WriterOutput::Memory(_) | WriterOutput::Writer { .. } => Ok(()),
            WriterOutput::Pipeline(pipeline) => {
                pipeline.finish()?;
                Ok(())
            }
        }
    }

    fn take_failure(&mut self) -> Option<Error> {
        self.failure.take()
    }
}

impl OutputTarget for WriterOutputSink<'_> {
    fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write(bytes)
    }

    fn finish_segment(&mut self) -> Result<()> {
        match self.output {
            WriterOutput::Memory(_) => Ok(()),
            WriterOutput::Writer { .. } => {
                self.flush()?;
                Ok(())
            }
            WriterOutput::Pipeline(pipeline) => pipeline.finish().map_err(Into::into),
        }
    }

    fn finish_document(&mut self) -> Result<()> {
        self.finish_output()
    }
}

impl Write for WriterOutputSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self.output {
            WriterOutput::Memory(buffer) => {
                buffer.get_or_insert_with(Vec::new).extend_from_slice(bytes);
                Ok(bytes.len())
            }
            WriterOutput::Writer { identifier, writer } => match writer.write(bytes) {
                Ok(count) => Ok(count),
                Err(error) => {
                    self.failure = stdio_sink_failure(*identifier, &error);
                    Err(error)
                }
            },
            WriterOutput::Pipeline(pipeline) => match pipeline.write(bytes) {
                Ok(()) => Ok(bytes.len()),
                Err(error) => {
                    let failure: Error = error.into();
                    let message = failure.to_string();
                    self.failure = Some(failure);
                    Err(std::io::Error::other(message))
                }
            },
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.output {
            WriterOutput::Memory(_) | WriterOutput::Pipeline(_) => Ok(()),
            WriterOutput::Writer { identifier, writer } => match writer.flush() {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.failure = stdio_sink_failure(*identifier, &error);
                    Err(error)
                }
            },
        }
    }
}

impl WriterOutput {
    fn take_memory(&mut self) -> Result<Vec<u8>> {
        match self {
            Self::Memory(buffer) => buffer.take().ok_or_else(|| {
                Error::Unsupported("get_buffer is only available once after a memory write".into())
            }),
            Self::Writer { .. } | Self::Pipeline(_) => Err(Error::Unsupported(
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

/// A reusable qpdf-shaped writer configuration.
///
/// qpdf's `QPDFJob::setWriterOptions` (`libqpdf/QPDFJob.cc:2847-2920`)
/// applies the same writer settings to every output writer created by a job.
/// Split-page jobs therefore need a configuration snapshot that can be
/// replayed on each fresh chunk writer instead of retaining only one setting
/// such as deterministic IDs. This type contains writer settings only; output
/// sinks and progress reporters remain owned by each [`PdfWriter`] and its
/// [`crate::job::QPDFJob`].
#[derive(Debug, Clone, Default)]
pub struct WriterConfiguration {
    settings: WriterSettings,
}

impl WriterConfiguration {
    /// Set qpdf's object-stream emission mode.
    pub fn set_object_stream_mode(&mut self, mode: ObjectStreamMode) {
        self.settings.object_stream_mode = mode;
    }

    /// Set qpdf's legacy stream-data policy.
    pub fn set_stream_data_mode(&mut self, mode: StreamDataMode) {
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

    /// Set qpdf's ordinary stream compression switch.
    pub fn set_compress_streams(&mut self, value: bool) {
        self.settings.compress_streams = value;
        self.settings.compress_streams_set = true;
    }

    /// Set qpdf's stream decode level.
    pub fn set_decode_level(&mut self, level: DecodeLevel) {
        self.settings.decode_level = level;
        self.settings.decode_level_set = true;
    }

    /// Set qpdf's `--recompress-flate` policy.
    pub fn set_recompress_flate(&mut self, value: bool) {
        self.settings.recompress_flate = value;
    }

    /// Set qpdf's Flate compression level for subsequently-created streams.
    ///
    /// The value is passed to the underlying qpdf-shaped Flate codec when it
    /// is non-negative. `0..=9` are the zlib compression levels. Values outside
    /// that range are retained and rejected lazily by zlib at stream
    /// initialization, where qpdf's writer can retry that stream unfiltered.
    /// Negative values select the codec default.
    pub fn set_compression_level(&mut self, level: i32) {
        self.settings.compression_level = Some(level);
    }

    /// Set qpdf's content-normalization policy.
    pub fn set_content_normalization(&mut self, value: bool) {
        self.settings.content_normalization = value;
        self.settings.content_normalization_set = true;
    }

    /// Set qpdf's QDF output mode.
    pub fn set_qdf_mode(&mut self, value: bool) {
        self.settings.qdf_mode = value;
    }

    /// Preserve otherwise unreferenced source objects.
    pub fn set_preserve_unreferenced_objects(&mut self, value: bool) {
        self.settings.preserve_unreferenced_objects = value;
    }

    /// Set qpdf's boolean `--newline-before-endstream` policy.
    pub fn set_newline_before_endstream(&mut self, value: bool) {
        self.settings.newline_before_endstream = if value {
            NewlineBeforeEndstream::Yes
        } else {
            NewlineBeforeEndstream::Never
        };
    }

    /// Set qpdf's minimum output PDF version and extension level.
    pub fn set_minimum_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) {
        update_minimum_pdf_version(
            &mut self.settings.minimum_pdf_version,
            version.into(),
            extension_level,
        );
    }

    /// Force qpdf's output PDF version and extension level.
    pub fn force_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) {
        self.settings.forced_pdf_version = Some((version.into(), extension_level));
    }

    /// Add qpdf's extra header text to each output writer.
    pub fn set_extra_header_text(&mut self, text: impl Into<String>) {
        let mut text = text.into();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        self.settings.extra_header_text = text;
    }

    /// Set qpdf's deterministic changing trailer ID policy.
    pub fn set_deterministic_id(&mut self, value: bool) {
        self.settings.deterministic_id = value;
    }

    /// Set qpdf's test-only static trailer ID policy.
    pub fn set_static_id(&mut self, value: bool) {
        self.settings.static_id = value;
    }

    /// Set qpdf's test-only static AES IV policy.
    pub fn set_static_aes_iv(&mut self, value: bool) {
        self.settings.static_aes_iv = value;
    }

    /// Set qpdf's QDF original-object-ID comment suppression policy.
    pub fn set_suppress_original_object_ids(&mut self, value: bool) {
        self.settings.suppress_original_object_ids = value;
    }

    /// Set whether source encryption may be preserved when compatible.
    pub fn set_preserve_encryption(&mut self, value: bool) {
        self.settings.preserve_encryption = value;
    }

    /// Configure explicit output encryption parameters.
    pub fn set_encryption_parameters(&mut self, params: EncryptParams) {
        self.settings.encryption_parameters = Some(params);
        self.settings.copy_encryption = None;
    }

    pub(crate) fn clear_encryption_parameters(&mut self) {
        self.settings.encryption_parameters = None;
        self.settings.copy_encryption = None;
    }

    /// The configured explicit output encryption parameters, if any.
    pub(crate) fn encryption_parameters(&self) -> Option<&EncryptParams> {
        self.settings.encryption_parameters.as_ref()
    }

    /// Whether qpdf can preserve source encryption at the writer boundary.
    ///
    /// This is the setting-only half of `QPDFWriter::doWriteSetup`: the
    /// attached document is checked separately by the writer, while qdf,
    /// content normalization, decoding, PCLm, and explicit encryption all
    /// disable implicit source preservation (`QPDFWriter.cc:1980-2048`). A
    /// `QPDFJob` uses this predicate when a multi-source page operation has
    /// replaced the encrypted primary with a fresh target and must carry the
    /// primary's encryption snapshot to `writeQPDF`.
    pub(crate) fn can_preserve_encryption(&self) -> bool {
        let mut options = self.settings.to_write_options();
        if self.settings.linearization {
            // qpdf clears QDF before selecting its linearized writer
            // (`QPDFWriter.cc:2036-2038`).
            options.qdf = false;
        }
        self.settings.preserve_encryption
            && self.settings.encryption_parameters.is_none()
            && self.settings.copy_encryption.is_none()
            && !options.qdf
            && !options.content_normalization
            && options.decode_level == DecodeLevel::None
            && !self.settings.pclm
    }

    /// Apply qpdf's `QPDFJob::maybeFixWritePassword` policy to configured
    /// encryption passwords before a writer emits its encryption dictionary.
    ///
    /// The returned vector contains one notice per configured password in
    /// qpdf's call order: user password first, owner password second.
    /// Password validation and conversion stay in the encryption password
    /// module so direct CLI writers and `QPDFJob` share one implementation.
    pub fn normalize_encryption_passwords(
        &mut self,
        password_mode: PasswordMode,
    ) -> Result<Vec<PasswordWriteNotice>> {
        let Some(params) = self.settings.encryption_parameters.as_mut() else {
            return Ok(Vec::new());
        };
        let revision = match params.method {
            EncryptMethod::V1Rc440 => 2,
            EncryptMethod::V2Rc4128 => 3,
            EncryptMethod::V4Aes128 | EncryptMethod::V4Rc4128 => 4,
            EncryptMethod::V5R5Aes256 => 5,
            EncryptMethod::V5R6Aes256 => 6,
        };
        let (user_password, user_notice) = crate::encryption::password::password_bytes_for_write(
            &params.user_password,
            password_mode,
            revision,
        )?;
        let (owner_password, owner_notice) = crate::encryption::password::password_bytes_for_write(
            &params.owner_password,
            password_mode,
            revision,
        )?;
        params.user_password = user_password;
        params.owner_password = owner_password;
        Ok(vec![user_notice, owner_notice])
    }

    /// Configure explicit encryption copied from an authenticated donor.
    pub fn copy_encryption_parameters(&mut self, source: CopyEncryptionSource) {
        self.settings.copy_encryption = Some(source);
        self.settings.encryption_parameters = None;
    }

    /// Set qpdf's linearized output mode.
    pub fn set_linearization(&mut self, value: bool) {
        self.settings.linearization = value;
        if value {
            self.settings.pclm = false;
        }
    }

    /// Set the optional qpdf linearization pass-one output path.
    pub fn set_linearization_pass1_filename(&mut self, path: impl Into<PathBuf>) {
        self.settings.linearization_pass1_filename = Some(path.into());
    }

    /// Apply this configuration to one writer while preserving its output
    /// sink lifecycle. Progress reporting is intentionally configured by the
    /// owning job after this method returns.
    pub fn apply_to<R: Read + Seek + 'static>(&self, writer: &mut PdfWriter<'_, R>) {
        writer.settings = self.settings.clone();
    }

    /// Return the stream decode level used by qpdf JSON serialization.
    ///
    /// qpdf keeps the JSON decode level beside the writer settings and uses
    /// the same value for `json` sections and writer-side stream policy.
    #[must_use]
    pub const fn decode_level(&self) -> DecodeLevel {
        self.settings.decode_level
    }

    /// Return whether otherwise-unreferenced objects are preserved by this
    /// writer configuration.
    #[must_use]
    pub const fn preserves_unreferenced_objects(&self) -> bool {
        self.settings.preserve_unreferenced_objects
    }

    /// Return qpdf's requested object-stream mode for job page selection.
    #[must_use]
    pub(crate) const fn object_stream_mode(&self) -> ObjectStreamMode {
        self.settings.object_stream_mode
    }
}

pub(crate) fn update_minimum_pdf_version(
    current: &mut Option<(String, i64)>,
    version: String,
    extension_level: i64,
) {
    let Some(candidate) = crate::pdf_version::parse_qpdf_writer_version(&version) else {
        // qpdf's parseVersion has no error channel for syntactically odd
        // values, but its integer conversion can overflow. Keep this
        // non-fallible public setter safe; job-option parsing rejects that
        // overflow before a writer is configured.
        return;
    };
    match current {
        None => *current = Some((version, extension_level)),
        Some((current_version, current_extension_level)) => {
            match crate::pdf_version::parse_qpdf_writer_version(current_version) {
                Some(current_parsed) => {
                    if candidate > current_parsed {
                        *current_version = version;
                        *current_extension_level = extension_level;
                    } else if candidate == current_parsed
                        && extension_level > *current_extension_level
                    {
                        // qpdf updates only the extension level on a numeric
                        // tie; the incumbent raw version spelling survives.
                        *current_extension_level = extension_level;
                    }
                }
                None => {
                    *current_version = version;
                    *current_extension_level = extension_level;
                }
            }
        }
    }
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
            .map_err(|error| Error::file_io("open", path.to_path_buf(), error))?;
        self.output = Some(WriterOutput::Writer {
            identifier: Some(QPDF_OUTPUT_PIPELINE),
            writer: Box::new(QpdfFileWriter::new(file)),
        });
        Ok(())
    }

    /// Configure an owned arbitrary writer sink.
    pub fn set_output_writer<W: Write + 'static>(&mut self, writer: W) -> Result<()> {
        self.ensure_output_unconfigured()?;
        self.output = Some(WriterOutput::Writer {
            identifier: None,
            writer: Box::new(writer),
        });
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

    /// Set qpdf's Flate compression level for subsequently-created streams.
    ///
    /// The value is passed to the underlying qpdf-shaped Flate codec when it
    /// is non-negative. `0..=9` are the zlib compression levels. Values outside
    /// that range are retained and rejected lazily by zlib at stream
    /// initialization, where qpdf's writer can retry that stream unfiltered.
    /// Negative values select the codec default.
    pub fn set_compression_level(&mut self, level: i32) {
        self.settings.compression_level = Some(level);
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
            NewlineBeforeEndstream::Yes
        } else {
            NewlineBeforeEndstream::Never
        };
    }

    pub fn set_minimum_pdf_version(&mut self, version: impl Into<String>, extension_level: i64) {
        update_minimum_pdf_version(
            &mut self.settings.minimum_pdf_version,
            version.into(),
            extension_level,
        );
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

    /// Register a qpdf progress callback.
    ///
    /// The callback's error is returned from [`PdfWriter::write`] at the
    /// progress event that raised it; it is not deferred until a completed
    /// write. This is the Rust equivalent of qpdf's exception propagation
    /// from QPDFWriter::ProgressReporter::reportProgress.
    pub fn register_progress_reporter(
        &mut self,
        reporter: Box<dyn FnMut(u8) -> crate::Result<()> + 'static>,
    ) {
        self.settings.progress_reporter = Some(ProgressReporter::new(reporter));
    }

    /// Return the effective header version before writing.
    pub fn get_final_version(&mut self) -> Result<String> {
        let options = self.prepared_write_options()?;
        Ok(effective_pdf_version(
            self.pdf.version(),
            &options,
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
        // qpdf's setWriterOptions calls Pl_Flate::setCompressionLevel only
        // from QPDFJob::writeQPDF's dispatch to the actual writer
        // (QPDFJob.cc:2840-2875), never from a version-only inspection path.
        // Applying it here (write-dispatch only), rather than inside
        // prepared_write_options, keeps get_final_version() from mutating
        // this process-wide codec state as a side effect of a read-only query.
        if let Some(level) = options.compression_level {
            if level >= 0 {
                Flate::set_compression_level(level)?;
            }
        }
        // QPDFWriter's constructor snapshots `pdf.getRoot().getObjGen()`
        // before `doWriteSetup`/`getObjectCount`
        // (`QPDFWriter.cc:53,2059-2065`). Resolve the trailer's Root handle
        // at the same writer-owned boundary so root-driven lazy diagnostics
        // precede the full xref-table walk; a missing Root remains a direct
        // null candidate and is reported later by the normal writer route.
        self.pdf.root_handle()?;
        let mut setup = build_writer_setup(self.pdf, &options)?;
        // Page-tree repair below mutates `self.pdf`'s object graph in place
        // (promoting direct /Kids leaves, cloning duplicate leaves) and is not
        // safe to retry from a partially-mutated state on failure. Close off
        // retry here, before that first mutating call, rather than after
        // configure_progress_for_pdf: everything from this point on follows
        // this function's own one-shot contract (see doc above).
        self.write_started = true;
        // qpdf's QPDFWriter::doWriteSetup runs initializeSpecialStreams before
        // QPDFWriter::write snapshots getObjectCount for progress
        // (QPDFWriter.cc:2114-2115, 2189-2193). The QDF, explicit
        // content-normalization, and non-none decode-level routes use the
        // same page-tree repair boundary, matching qpdf's
        // qdf_mode || normalize_content || stream_decode_level trigger. Keep
        // the repaired page order and its three derived maps together so the
        // specialized emitter consumes this setup without another page walk.
        let special_streams = initialize_special_streams(self.pdf, &options)?;
        let effective_object_streams = effective_object_stream_mode(&options);
        if effective_object_streams == ObjectStreamMode::Generate {
            // qpdf initializes special streams before Generate computes its
            // compressible membership (`QPDFWriter.cc:2114-2139`). Capture
            // this snapshot at the same boundary, still before the common
            // `prepareFileForWrite` call below. Its fresh ObjStm placeholders
            // are also allocated before getObjectCount so they contribute to
            // the progress denominator (`QPDFWriter.cc:1998-2004,2189-2195`).
            // qpdf reaches `generateObjectStreams` through a bare
            // `switch (m->object_stream_mode)` that no QDF, encryption, PCLm,
            // or linearization predicate guards (`QPDFWriter.cc:2125-2139`),
            // so every Generate route captures here. That boundary is what
            // preserves objects such as an indirect `/Extensions` dictionary:
            // `prepareFileForWrite` directizes it afterwards, but the object
            // has already been assigned to an ObjStm.
            let compressible = object_streams::compressible_objgens_qpdf_plan(self.pdf)?;
            let generated_object_stream_count =
                object_streams::even_split_into_streams(&compressible.eligible).len();
            setup.generated_compressible = Some(compressible);
            for _ in 0..generated_object_stream_count {
                let placeholder = self
                    .pdf
                    .make_indirect_from_object_handle(ObjectHandle::null())?;
                let source = placeholder.object_ref().ok_or_else(|| {
                    // cov:ignore-start: make_indirect_from_object_handle always returns an indirect handle
                    Error::Internal(
                        "generated ObjStm placeholder has no source identity".to_string(),
                    )
                    // cov:ignore-end
                })?; // cov:ignore: make_indirect_from_object_handle guarantees a source identity for a generated placeholder.
                setup.generated_object_stream_sources.push(source);
            }
        }
        if self.settings.linearization && special_streams.is_none() {
            // qpdf filters linearized page dictionaries after Preserve/Generate
            // setup and before its object-count snapshot
            // (QPDFWriter.cc:2125-2149,2189-2195). Keep this page walk after
            // object-stream membership calculation and before the common
            // writer preparation boundary.
            crate::pages::repair::prepare_for_optimization(self.pdf)?;
        }
        // qpdf snapshots `getObjectCount()` for every write, even when no
        // progress reporter is configured (`QPDFWriter.cc:2189-2195`). That
        // call is also the document-owned `fixDanglingReferences` boundary,
        // so skipping it on the no-progress route changes lazy warning timing
        // and the order in which malformed xref objects are resolved.
        self.pdf.get_object_count()?;
        crate::writer::configure_progress_for_pdf(
            self.pdf,
            &options,
            0,
            self.settings.linearization,
        )?; // cov:ignore: progress tests exercise this call; LLVM maps its successful multiline continuation to an unhit region

        // QPDFWriter::write performs the one writer-owned graph preparation
        // after the progress object-count snapshot and before it chooses
        // writeLinearized or writeStandard (QPDFWriter.cc:2187-2200).
        // Keep fixDanglingReferences and Catalog extension directization on
        // this common boundary so the two output routes consume one live
        // graph rather than maintaining route-local preparation bridges.
        prepare_file_for_write(self.pdf)?;
        let result = if self.settings.linearization {
            options.qdf = false;
            let pass1_path = self.settings.linearization_pass1_filename.as_deref();
            let output = self
                .output
                .as_mut()
                .expect("output was checked before writing");
            let mut target = WriterOutputSink::new(output);
            match write_linearized_for_pdf_writer(
                self.pdf,
                &options,
                pass1_path,
                setup,
                special_streams.as_ref(),
                &mut target,
            ) {
                Ok(result) => result,
                Err(error) => return Err(target.take_failure().unwrap_or(error)),
            }
        } else {
            let output = self
                .output
                .as_mut()
                .expect("output was checked before writing");
            let mut target = WriterOutputSink::new(output);
            let emission = {
                let mut sink = OutputSink::new(&mut target);
                match emit_canonical_pdf_with_special_streams(
                    self.pdf,
                    &mut sink,
                    &options,
                    special_streams.as_ref(),
                    setup,
                ) {
                    Ok(result) => sink.finish_document().map(|()| result),
                    Err(error) => Err(error),
                }
            };
            match emission {
                Ok(result) => result,
                Err(error) => return Err(target.take_failure().unwrap_or(error)),
            }
        };

        report_progress_finished(&options)?;
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
            // qpdf's doWriteSetup makes PCLm cleartext with decode disabled
            // and compression disabled before source-encryption preservation
            // is considered, and touches nothing else: QDF mode and the
            // object-stream mode stay exactly as configured
            // (`QPDFWriter.cc:2071-2076`). Explicit content normalization
            // remains active; willFilterStream applies it only to selected
            // page-content streams (`QPDFWriter.cc:2114-2115`).
            options.encrypt = None;
            options.copy_encryption = None;
            options.decode_level = DecodeLevel::None;
            options.compress_streams = crate::CompressStreams::No;
            options.stream_data = None;
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

        // QPDFWriter::setEncryptionParameters and
        // QPDFWriter::copyEncryptionParameters both call generateID() before
        // installing the encryption state (QPDFWriter.cc:619 and :656). A
        // deterministic ID has no data until the writer has emitted the bytes,
        // so qpdf reports generateID's logic_error for this combination before
        // forced-version handling can disable encryption. With a static ID
        // that early generateID succeeds and pushMD5Pipeline rejects the
        // already-generated ID instead (QPDFWriter.cc:1011-1014).
        if options.deterministic_id
            && (options.encrypt.is_some() || options.copy_encryption.is_some())
        {
            return Err(deterministic_id_encryption_error(&options));
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

/// qpdf's `disableIncompatibleEncryption` for the writer options that have
/// reached this bridge. A forced version is a hard cap: when it cannot
/// represent the selected Standard security handler, qpdf silently drops
/// encryption and writes the rewritten objects in cleartext.
fn forced_version_disables_encryption(options: &WriterOptions) -> bool {
    let Some(forced) = options
        .force_version
        .as_deref()
        .filter(|version| !version.is_empty())
        .and_then(crate::pdf_version::parse_qpdf_writer_version)
    else {
        return false;
    };

    let Some((version, revision, use_aes)) = encryption_shape(options) else {
        return false;
    };

    let v = QpdfVersionParts::new(1, 3);
    if forced < v {
        return true;
    }
    let v = QpdfVersionParts::new(1, 4);
    if forced < v && (version > 1 || revision > 2) {
        return true;
    }
    let v = QpdfVersionParts::new(1, 5);
    if forced < v && (version > 2 || revision > 3) {
        return true;
    }
    let v = QpdfVersionParts::new(1, 6);
    if forced < v && use_aes {
        return true;
    }
    let v = QpdfVersionParts::new(1, 7);
    (forced < v || (forced == v && options.force_extension_level.unwrap_or(0) < 3))
        && (version >= 5 || revision >= 5)
}

fn encryption_shape(options: &WriterOptions) -> Option<(i64, i64, bool)> {
    if let Some(params) = options.encrypt.as_ref() {
        use crate::encryption::EncryptMethod;
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
    let version = source
        .encrypt_dict
        .try_get_key(b"/V")
        .ok()?
        .try_as_integer()
        .ok()??;
    let revision = source
        .encrypt_dict
        .try_get_key(b"/R")
        .ok()?
        .try_as_integer()
        .ok()??;
    Some((version, revision, version >= 4))
}

/// Result data produced by a completed full-rewrite emitter.
///
/// Both maps are assembled while writing the output. They deliberately do not
/// consult the source xref table: the caller observes the objects and xref
/// records that this emitter actually placed in the new file.
#[derive(Clone, Debug, Default)]
pub(crate) struct WriterResult {
    pub(crate) old_to_new: BTreeMap<ObjectRef, ObjectRef>,
    pub(crate) written_xref: BTreeMap<ObjectRef, XrefEntry>,
}

impl WriterResult {
    pub(crate) fn new(
        old_to_new: BTreeMap<ObjectRef, ObjectRef>,
        written_xref: BTreeMap<ObjectRef, XrefEntry>,
    ) -> Self {
        Self {
            old_to_new,
            written_xref,
        }
    }
}

/// Controls whether the full-rewrite path applies FlateDecode compression to
/// output streams.
///
/// # Byte-vs-observable policy
///
/// flpdf uses zlib (via the `flate2` crate) with `Compression::default()`,
/// which selects a different compression level and block layout than qpdf's
/// internal zlib build.  As a result, **flpdf's FlateDecode output is
/// observably equivalent to qpdf's (same decoded bytes) but will not be
/// byte-identical**.  The acceptance criterion for this toggle is round-trip
/// correctness (decoded bytes match), not byte-identical agreement with qpdf.
///
/// This tradeoff is intentional and documented here to avoid spending time
/// chasing byte-level zlib parity, which would require re-implementing qpdf's
/// exact compression parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressStreams {
    /// Apply FlateDecode to every output stream whose declared filter chain is
    /// decodable at the configured `WriterOptions::decode_level`. A chain that
    /// is unsupported or above that level is passed through unchanged.
    ///
    /// For the full-rewrite path this means: decode the source stream through
    /// its declared filter pipeline and re-emit the result with a single
    /// `/FlateDecode` filter. Streams whose decode is unavailable at the
    /// selected level or whose decode/re-encode fails are emitted verbatim.
    ///
    /// This is the default — matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    #[default]
    Yes,
    /// Emit every output stream without any FlateDecode compression.
    ///
    /// For the full-rewrite path: decode the source stream and write the raw
    /// bytes without any `/Filter`.  Streams whose decode fails (e.g. because
    /// the declared filter is `DCTDecode` / `JPXDecode` and the image data is
    /// opaque to flpdf) are passed through verbatim — their original `/Filter`
    /// chain is preserved so the output remains readable.
    No,
}

/// Controls how the full-rewrite path handles stream data.
///
/// This is the higher-level policy that mirrors qpdf's `--stream-data` option.
/// When configured on [`PdfWriter`], it **overrides** the writer's compression setting
/// for regular indirect streams (non-xref, non-ObjStm container bodies).
///
/// # Semantics
///
/// | Variant      | Equivalent `CompressStreams` | Behaviour |
/// |-------------|-------------------------------|-----------|
/// | `Preserve`  | bypass (no decode/re-encode)  | Pass dict + raw data verbatim; the canonical writer filter route is not called |
/// | `Uncompress`| `CompressStreams::No`         | Decode through all declared filters, emit raw bytes without any `/Filter` |
/// | `Compress`  | `CompressStreams::Yes`        | Decode, then re-encode with a single `/FlateDecode` filter |
///
/// # Interaction with `--compress-streams`
///
/// When `PdfWriter` is configured with a stream-data mode, it takes precedence
/// over the writer's compression setting for per-object stream bodies.
/// Linearized output also applies the resulting global compression choice to
/// its generated hint, object, and cross-reference streams, matching qpdf.
///
/// # Interaction with QDF mode
///
/// When [`PdfWriter`] is configured for QDF, QDF wins: every applicable stream is
/// decoded to raw bytes (equivalent to `Uncompress`), overriding even
/// `stream_data = Some(Preserve)`.  This matches qpdf's behaviour where `--qdf`
/// takes precedence over `--stream-data=preserve`.
///
/// # Default
///
/// The default is `None` — no stream-data mode is set — which leaves the
/// writer's compression setting in control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamDataMode {
    /// Pass streams through verbatim — no decode or re-encode.
    ///
    /// The stream dictionary and raw data bytes are emitted unchanged.  This
    /// bypasses the canonical stream filter route entirely, so a stream carrying
    /// `/Filter /FlateDecode` will still carry that filter in the output.
    Preserve,
    /// Decode and emit raw bytes without any `/Filter`.
    ///
    /// Equivalent to `CompressStreams::No`: the declared filter chain is decoded
    /// and the raw bytes are written without any `/Filter` or `/DecodeParms`.
    /// Streams that cannot be decoded (e.g. DCTDecode) are emitted verbatim.
    Uncompress,
    /// Decode and re-encode with a single `/FlateDecode` filter.
    ///
    /// Equivalent to `CompressStreams::Yes`: the declared filter chain is decoded
    /// and the result is re-encoded with FlateDecode.
    Compress,
}

/// Compute the effective stream policy for regular indirect streams.
///
/// Returns `Some(policy)` meaning "the canonical writer stream route should
/// apply this policy", or `None` meaning "preserve mode: skip decode/re-encode
/// and emit the stream verbatim".
///
/// # Priority
///
/// 1. Legacy QDF mode (`options.qdf`) returns `Some(CompressStreams::No)` —
///    QDF requires fully decoded streams regardless of `stream_data`. The
///    PdfWriter bridge precomputes qpdf's setter-aware QDF defaults instead.
/// 2. `options.stream_data = Some(mode)` overrides `options.compress_streams`.
/// 3. `options.stream_data = None` falls back to `options.compress_streams`.
pub(crate) fn effective_stream_policy(options: &WriterOptions) -> Option<CompressStreams> {
    if options.qdf && !options.qdf_stream_policy_precomputed {
        return Some(CompressStreams::No);
    }
    match options.stream_data {
        Some(StreamDataMode::Preserve) => None,
        Some(StreamDataMode::Uncompress) => Some(CompressStreams::No),
        Some(StreamDataMode::Compress) => Some(CompressStreams::Yes),
        None => Some(options.compress_streams),
    }
}

/// Controls whether a newline is inserted immediately before the `endstream`
/// keyword.
///
/// ISO 32000-1 §7.3.8.1 recommends an end-of-line marker before `endstream`.
/// In all variants the `/Length` dictionary entry reflects the raw payload
/// length only — never any inserted newline.
///
/// # Variants and qpdf equivalence
///
/// - [`Never`](Self::Never) (the **flpdf default**) — never insert a newline;
///   exactly the raw payload bytes sit between `stream` and `endstream`. This
///   reproduces qpdf's **default** output (qpdf only inserts a newline when run
///   with `--newline-before-endstream`), and is required for byte-identical
///   `qpdf`-equivalent output.
/// - [`Yes`](Self::Yes) — always write exactly one `b'\n'`, satisfying the ISO
///   32000-1 §7.3.8.1 recommendation and easing hand-editing. Equivalent to
///   qpdf run **with** `--newline-before-endstream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum NewlineBeforeEndstream {
    /// Always write exactly one `b'\n'` before `endstream`, regardless of
    /// whether the payload already ends with a newline.
    ///
    /// Satisfies ISO 32000-1 §7.3.8.1 and matches qpdf run with
    /// `--newline-before-endstream`.
    Yes,
    /// Never insert a newline: the raw payload is written verbatim and
    /// `endstream` follows immediately, so exactly `/Length` bytes sit between
    /// `stream` and `endstream` (the **flpdf default**).
    ///
    /// Reproduces qpdf's default output and is required for byte-identical
    /// qpdf-equivalent rewrites.
    #[default]
    Never,
}

/// Fixed V=5 R=5/R=6 secret material for qpdf-compatible test/helper writes.
///
/// This type is compiled only for crate unit tests and the `qpdf-zlib-compat`
/// test feature. The byte order matches qpdf 11.9.0's four random draws:
/// 32-byte file key, 16 bytes of `/U` salts, 16 bytes of `/O` salts, and the
/// 4-byte `/Perms` tail. Production writes do not expose this field and keep
/// using the OS CSPRNG.
#[cfg(any(test, feature = "qpdf-zlib-compat"))]
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V5Randomness {
    /// 32-byte file encryption key.
    pub file_key: [u8; 32],
    /// 8-byte user-password validation salt.
    pub user_validation_salt: [u8; 8],
    /// 8-byte user-password key-derivation salt.
    pub user_key_salt: [u8; 8],
    /// 8-byte owner-password validation salt.
    pub owner_validation_salt: [u8; 8],
    /// 8-byte owner-password key-derivation salt.
    pub owner_key_salt: [u8; 8],
    /// 4 bytes appended to the `/Perms` plaintext block.
    pub perms_random_tail: [u8; 4],
}

#[cfg(any(test, feature = "qpdf-zlib-compat"))]
impl V5Randomness {
    /// Split one qpdf-ordered 68-byte random input into the V=5 fields.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 68]) -> Self {
        Self {
            file_key: std::array::from_fn(|index| bytes[index]),
            user_validation_salt: std::array::from_fn(|index| bytes[32 + index]),
            user_key_salt: std::array::from_fn(|index| bytes[40 + index]),
            owner_validation_salt: std::array::from_fn(|index| bytes[48 + index]),
            owner_key_salt: std::array::from_fn(|index| bytes[56 + index]),
            perms_random_tail: std::array::from_fn(|index| bytes[64 + index]),
        }
    }
}

/// Shared callback storage used by the qpdf-shaped writer lifecycle.
///
/// `WriterOptions` is cloneable because the full-rewrite preflight creates
/// short-lived option snapshots. Keeping the callback behind shared interior
/// mutability preserves that property while still allowing each snapshot to
/// report to the one registered qpdf progress reporter. The callback is
/// fallible so a pipeline failure can abort the active writer like qpdf's
/// uncaught progress-reporter exception.
type ProgressCallback = Box<dyn FnMut(u8) -> crate::Result<()> + 'static>;
type SharedProgressCallback = Rc<RefCell<ProgressCallback>>;
type SharedProgressState = Rc<RefCell<ProgressStateInner>>;

#[derive(Clone)]
pub(crate) struct ProgressReporter {
    callback: SharedProgressCallback,
    state: SharedProgressState,
}

impl ProgressReporter {
    pub(crate) fn new(reporter: Box<dyn FnMut(u8) -> crate::Result<()> + 'static>) -> Self {
        Self {
            callback: Rc::new(RefCell::new(reporter)),
            state: Rc::new(RefCell::new(ProgressStateInner::default())),
        }
    }

    pub(crate) fn report(&self, percent: u8) -> crate::Result<()> {
        (self.callback.borrow_mut())(percent)
    }

    pub(crate) fn configure(&self, events_expected: usize) {
        *self.state.borrow_mut() = ProgressStateInner {
            events_expected: events_expected.max(1),
            ..ProgressStateInner::default()
        };
    }

    /// Translate QPDFWriter::indicateProgress (`QPDFWriter.cc:2957-2982`).
    ///
    /// The counter is shared because the canonical writer clones its option
    /// snapshot while a linearized file performs both passes. The callback is
    /// invoked after the state borrow is released so a reporter can safely
    /// observe external state without extending the writer's interior borrow.
    pub(crate) fn indicate(&self, decrement: bool, finished: bool) -> crate::Result<()> {
        let progress = {
            let mut state = self.state.borrow_mut();
            if decrement {
                state.events_seen = state.events_seen.saturating_sub(1);
                return Ok(());
            }

            state.events_seen = state.events_seen.saturating_add(1);
            let progress = if finished {
                Some(100)
            } else if state.events_seen >= state.next_progress_report {
                Some(if state.next_progress_report == 0 {
                    0
                } else {
                    let scaled = state.events_seen.saturating_mul(100) / state.events_expected;
                    1_u8.saturating_add(u8::try_from(scaled.min(98)).unwrap_or(98))
                })
            } else {
                None
            };

            let increment = (state.events_expected / 100).max(1);
            while state.events_seen >= state.next_progress_report {
                state.next_progress_report = state.next_progress_report.saturating_add(increment);
            }
            progress
        };

        if let Some(progress) = progress {
            self.report(progress)?;
        }
        Ok(())
    }
}

impl fmt::Debug for ProgressReporter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProgressReporter(..)")
    }
}

#[derive(Debug)]
struct ProgressStateInner {
    events_expected: usize,
    events_seen: usize,
    next_progress_report: usize,
}
impl Default for ProgressStateInner {
    fn default() -> Self {
        Self {
            events_expected: 1,
            events_seen: 0,
            next_progress_report: 0,
        }
    }
}

/// Internal options shared by the canonical writer and writer-owned tests.
/// Public callers configure [`PdfWriter`] directly.
#[derive(Debug, Default, Clone)]
pub(crate) struct WriterOptions {
    /// Stream decode level used by the qpdf-shaped writer bridge.
    ///
    /// A filter chain that is not wholly decodable at this level is preserved
    /// as a whole, matching QPDF_Stream's all-or-nothing filterability
    /// decision.
    pub decode_level: DecodeLevel,

    /// Normalize decoded page-content streams using qpdf token rules.
    ///
    /// This applies to direct streams in a page `/Contents` value and terminal
    /// indirect streams reached from page `/Contents`; other streams retain
    /// their decoded bytes unchanged. PdfWriter enables it implicitly for QDF
    /// output unless the caller explicitly disables it.
    pub content_normalization: bool,

    /// Override the trailer `/ID`'s second element (the changing identifier)
    /// with qpdf's static-id constant — the first 32 hex digits of π. The
    /// first element (the permanent identifier) is preserved from the input
    /// trailer when present; if absent, both elements are set to the constant.
    /// Mirrors `qpdf --static-id` and is intended for byte-identical testing.
    pub static_id: bool,

    /// Derive the trailer `/ID[1]` (the changing identifier) with qpdf's
    /// two-seed deterministic-ID algorithm. The first MD5 is an incremental
    /// digest of every final-output byte through the opening `/ID [` marker:
    /// this includes a classic xref table and the xref-stream dictionary, but
    /// excludes the identifier bytes themselves and the xref-stream payload
    /// after that cutoff. The second MD5 hashes qpdf's hex first digest plus
    /// ` QPDF ` and the decoded `/Info` string values, truncated at the first
    /// NUL byte. The permanent identifier `/ID[0]` is preserved from the input
    /// (ISO 32000-1 §14.4), falling back to the second digest when the input has
    /// no usable `/ID`. This mirrors `qpdf --deterministic-id` byte-for-byte.
    ///
    /// The canonical rewrite honors this flag. When it is combined with
    /// [`WriterOptions::static_id`], the static ID takes precedence for the
    /// emitted `/ID`, matching qpdf's `QPDFWriter::generateID` check order.
    /// Any output with `deterministic_id` enabled remains rejected for
    /// encryption because qpdf still enables its deterministic digest pipeline
    /// and the encryption setup generates an ID before that pipeline exists.
    ///
    pub deterministic_id: bool,

    /// Force every AES CBC IV to `0x00 × 16` instead of a cryptographically
    /// random value.
    ///
    /// **TESTING ONLY — NOT for production.**  When `true`, both stream-level
    /// and string-level AES encryption use the same fixed initialization
    /// vector qpdf's `--static-aes-iv` uses — byte `i` is `14 * (1 + i)` —
    /// making the ciphertext deterministic and comparable with qpdf's output.
    /// Without this flag (the default `false`) every encryption call generates
    /// a fresh random IV via the OS CSPRNG.
    ///
    /// Under CBC the vector is written at the head of the ciphertext, so it is
    /// part of the output bytes: a different vector means a different file.
    /// Must never be set in production code; deterministic IVs make AES CBC
    /// completely insecure.
    pub static_aes_iv: bool,

    /// Fixed V=5 security-handler random bytes for qpdf byte-gate helpers.
    ///
    /// This field exists only in crate unit tests and builds with the
    /// `qpdf-zlib-compat` test feature. It is deliberately not a CLI or
    /// production seed option. `None` preserves the production OS CSPRNG.
    #[cfg(any(test, feature = "qpdf-zlib-compat"))]
    #[doc(hidden)]
    pub v5_randomness: Option<V5Randomness>,

    /// Enforce a minimum PDF version in the output header.
    ///
    /// The effective version is `max(source_version, min_version)`. Format:
    /// `"1.3"`, `"1.7"`, etc.
    ///
    /// Mirrors `qpdf --min-version`.
    pub min_version: Option<String>,

    /// Enforce a minimum Adobe extension level in the output catalog's
    /// `/Extensions /ADBE /ExtensionLevel`.
    ///
    /// Combined with [`Self::min_version`] via qpdf's pairwise rule: a higher
    /// `min_version` **resets** the extension level (does not carry it across a
    /// version bump). When the resulting effective level is greater than 0, the
    /// writer injects
    /// `/Extensions << /ADBE << /BaseVersion /<ver> /ExtensionLevel <lvl> >> >>`
    /// into the Catalog on the full-rewrite path. When 0, no injection (existing
    /// Catalog untouched).
    ///
    /// Mirrors qpdf's `--min-version <version>-<level>` (the level portion) and
    /// the extension_level `QPDFJob` accumulates into `max_input_version` from
    /// every opened input's Catalog.
    pub min_extension_level: Option<i64>,

    /// Force the output PDF version header to exactly this value, ignoring the
    /// source version and other minimum-version floors.
    ///
    /// Mirrors `qpdf --force-version`.
    pub force_version: Option<String>,

    /// Adobe extension level paired with [`Self::force_version`].
    ///
    /// qpdf treats the version and extension level as one forced pair when
    /// deciding whether encryption remains compatible and when reconciling
    /// the Catalog's `/Extensions /ADBE` entry.
    pub force_extension_level: Option<i64>,

    /// Text written immediately after qpdf's binary or PCLm header marker.
    ///
    /// [`PdfWriter::set_extra_header_text`](crate::PdfWriter::set_extra_header_text)
    /// normalizes this value to end in one newline, matching qpdf.
    pub extra_header_text: String,

    /// When `true`, suppress the `%% Original object ID: N M` comments that the
    /// QDF writer would otherwise emit before each object.
    ///
    /// Mirrors `qpdf --no-original-object-ids`. qpdf's own help: *"Omit
    /// comments in a QDF file indicating the object ID an object had in the
    /// original file."* Observed against qpdf 11.9.0, this flag affects **only**
    /// QDF output (`qpdf --qdf` vs `qpdf --qdf --no-original-object-ids`); JSON
    /// v1 and v2 output are byte-identical with or without it, so this field is
    /// intentionally **not** wired into any JSON path.
    ///
    /// The canonical rewrite emits `%% Original object ID: N G` immediately
    /// before each indirect object's `N G obj` line when `qdf = true` and this
    /// flag is `false`. Setting this flag to `true` suppresses those comments
    /// while leaving the `N G obj` lines intact — matching qpdf's
    /// `--no-original-object-ids` behaviour exactly.
    pub no_original_object_ids: bool,

    /// Object stream emission policy for the output.
    ///
    /// Mirrors `qpdf --object-streams=preserve|disable|generate`. Defaults to
    /// [`ObjectStreamMode::Preserve`], matching qpdf's behaviour for a plain
    /// `qpdf in.pdf out.pdf` invocation.
    ///
    /// The canonical rewrite consults this setting whenever it emits ObjStms.
    pub object_streams: ObjectStreamMode,

    /// Preserve source objects that are not reachable from the trailer roots.
    ///
    /// The plain emitter honors it across [`ObjectStreamMode::Disable`],
    /// [`ObjectStreamMode::Preserve`], and [`ObjectStreamMode::Generate`]
    /// planning, while still excluding explicitly removed identities.
    pub preserve_unreferenced_objects: bool,

    /// Stream compression policy for the full-rewrite path.
    ///
    /// [`CompressStreams::Yes`] (the default) decodes each stream and
    /// re-encodes it with a single `/FlateDecode` filter, matching qpdf's
    /// default behaviour.  [`CompressStreams::No`] decodes each stream and
    /// emits the raw bytes without any filter; streams that cannot be decoded
    /// at the selected level or whose data is corrupt are passed through
    /// verbatim.
    ///
    /// It governs regular indirect streams, ObjStm containers, and the xref
    /// stream alike.
    pub compress_streams: CompressStreams,

    /// Whether to insert a newline immediately before each `endstream` keyword.
    ///
    /// ISO 32000-1 §7.3.8.1 recommends an end-of-line marker before `endstream`.
    /// [`NewlineBeforeEndstream::Never`] (the default) never inserts one, so
    /// exactly `/Length` bytes sit between `stream` and `endstream` — matching
    /// qpdf's default output and required for byte-identical qpdf-equivalent
    /// rewrites. [`NewlineBeforeEndstream::Yes`] always writes exactly one
    /// `b'\n'` before `endstream`, matching qpdf run with
    /// `--newline-before-endstream`. QDF applies qpdf's separate conditional
    /// rule: it adds a newline only when the payload's last byte is not `\n`.
    ///
    /// The `/Length` value in the stream dictionary is **not** affected by this
    /// setting — it always reflects the raw payload byte count only.
    ///
    /// Applied to every stream in the canonical rewrite output.
    pub newline_before_endstream: NewlineBeforeEndstream,

    /// Emit the document in QDF (Query Data Format) mode.
    ///
    /// When `true`, every stream that uses a
    /// "safe text" filter chain — [`FlateDecode`], [`LZWDecode`], [`ASCIIHexDecode`],
    /// [`ASCII85Decode`], [`RunLengthDecode`] — is fully decoded and written as raw
    /// bytes.  The `/Filter` and `/DecodeParms` entries are removed from the stream
    /// dictionary and `/Length` is updated to the decoded byte count, making the
    /// stream data human-readable in a text editor.
    ///
    /// Image/binary codecs that flpdf cannot decompress — `DCTDecode`, `JBIG2Decode`,
    /// `JPXDecode`, `CCITTFaxDecode` — and any unknown filter are left **untouched**:
    /// the compressed bytes and the original `/Filter` chain are preserved verbatim.
    /// This matches qpdf's own QDF behaviour.
    ///
    /// When `true`, this setting takes precedence over [`compress_streams`] for the
    /// per-object stream emission: the stream is always emitted decompressed regardless
    /// of the `compress_streams` value.  The xref stream and ObjStm containers are
    /// governed solely by `compress_streams` and are not affected by this flag.
    ///
    /// This field is the internal emitter representation of QDF mode.
    ///
    /// [`FlateDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`LZWDecode`]: https://pdf.pizza/spec/7.4.4
    /// [`ASCIIHexDecode`]: https://pdf.pizza/spec/7.4.2
    /// [`ASCII85Decode`]: https://pdf.pizza/spec/7.4.3
    /// [`RunLengthDecode`]: https://pdf.pizza/spec/7.4.5
    /// [`compress_streams`]: WriterOptions::compress_streams
    pub qdf: bool,

    /// Whether the PdfWriter lifecycle already applied qpdf's setter-aware
    /// QDF stream defaults.
    pub(crate) qdf_stream_policy_precomputed: bool,

    /// Higher-level stream data policy (qpdf `--stream-data={preserve,uncompress,compress}`).
    ///
    /// When set, this overrides [`compress_streams`] for regular indirect stream bodies.
    /// Structural streams (xref streams and ObjStm containers) are not affected and
    /// continue to use [`compress_streams`].
    ///
    /// | Value                          | Effect on regular streams            |
    /// |-------------------------------|--------------------------------------|
    /// | `None` (default)              | Fall back to `compress_streams`      |
    /// | `Some(StreamDataMode::Preserve)` | Emit dict + raw bytes verbatim    |
    /// | `Some(StreamDataMode::Uncompress)` | Decode, emit raw (no `/Filter`) |
    /// | `Some(StreamDataMode::Compress)`   | Decode, re-encode with FlateDecode |
    ///
    /// **Note:** when `qdf = true`, QDF takes precedence over every `stream_data`
    /// value (including `Preserve`) and forces decoded output.
    ///
    /// **Note:** JSON output paths (`json_inspect`) are not yet wired to this field;
    /// only the full-rewrite path is affected (tracked separately).
    ///
    /// [`compress_streams`]: WriterOptions::compress_streams
    pub stream_data: Option<StreamDataMode>,

    /// Re-encode streams that are already a lone `/FlateDecode`.
    ///
    /// By default (`false`) a stream whose source filter is a single
    /// `/FlateDecode` is emitted **verbatim** under [`CompressStreams::Yes`] —
    /// its already-compressed bytes are preserved rather than decoded and
    /// re-encoded. This mirrors qpdf, which does not recompress a lone-Flate
    /// stream unless `--recompress-flate` is given.
    ///
    /// Set to `true` to force such streams through a decode + re-encode pass
    /// (equivalent to `qpdf --recompress-flate`). Has no effect under
    /// [`CompressStreams::No`] / [`StreamDataMode::Uncompress`] (which always
    /// decode) or [`StreamDataMode::Preserve`] (which never decodes).
    ///
    /// A lone-Flate stream that carries an external-file reference (`/F`) is
    /// always re-encoded regardless of this flag: its in-body bytes are not the
    /// canonical data, so they are never preserved verbatim.
    pub recompress_flate: bool,

    /// Optional qpdf Flate compression level. Negative values leave the codec
    /// default selected; non-negative values are passed to the Flate codec
    /// before output streams are created.
    pub compression_level: Option<i32>,

    /// Encrypt the canonical output with the supplied [`crate::EncryptParams`]
    /// (qpdf `--encrypt …` equivalent).
    ///
    /// When set the writer:
    ///
    /// 1. Resolves `/ID[0]` upfront (preserving the input's permanent
    ///    identifier when present, generating a fresh one otherwise) so
    ///    Algorithm 2 can derive the file encryption key from it.
    /// 2. Builds the `/Encrypt` dictionary via the algorithm-specific
    ///    builder (`build_v4_encrypt_dict` for the V=4 AES-128 walking
    ///    skeleton).
    /// 3. Encrypts every string in every emitted object (per-object key
    ///    via Algorithm 1) and every stream payload (with random AES IV
    ///    prepended + PKCS#7 padding, `/Length` updated to match).
    /// 4. Emits the `/Encrypt` dictionary itself as a plaintext indirect
    ///    object whose number is referenced from the trailer.
    ///
    /// **Required flag combinations** (the writer currently rejects others):
    ///
    /// - `qdf` may be enabled; encrypted strings and stream dictionaries retain
    ///   QDF layout while their encrypted bytes remain ciphertext.
    pub encrypt: Option<crate::encryption::EncryptParams>,

    /// Copy the authenticated encryption parameters from a donor PDF and
    /// re-use its file encryption key (qpdf `--copy-encryption`
    /// equivalent).
    ///
    /// When set the writer bypasses the normal password-derivation path and
    /// constructs an `EncryptionContext` directly from the pre-recovered file
    /// key, the donor's Standard handler values, and the donor's `/ID[0]`.
    /// qpdf's canonical copy rules are applied: V4 is emitted as AESV2 even
    /// when the donor used RC4, and V5 is emitted as AESV3.
    ///
    /// Exactly one of `encrypt` and `copy_encryption` may be set; the CLI
    /// enforces mutual exclusion via `conflicts_with`.  The writer asserts this
    /// invariant at the top of the full-rewrite path.
    ///
    /// V=1/V=2 RC4, V=4 AESV2, and V=5 R=5/R=6 AESV3 donors are supported by
    /// the canonical writer.
    pub copy_encryption: Option<crate::encryption::CopyEncryptionSource>,

    /// Emit qpdf's PCLm-oriented object order and header.
    pub pclm: bool,

    /// qpdf progress callback shared by the lifecycle bridge and the emitter.
    pub(crate) progress_reporter: Option<ProgressReporter>,
}

/// The page-derived state created by qpdf's `initializeSpecialStreams`.
///
/// qpdf builds these maps from one repaired page snapshot before it starts
/// numbering or emitting objects. Keep the snapshot and all derived maps
/// together so the specialized writer does not repair or enumerate the page
/// tree again while it is preparing QDF markers and stream policy.
#[derive(Debug, Default)]
pub(crate) struct SpecialStreams {
    pages: Vec<ObjectRef>,
    page_seq: HashMap<ObjectRef, u32>,
    contents_seq: HashMap<ObjectRef, u32>,
    normalized_streams: BTreeSet<ObjectRef>,
    normalized_streams_raw: BTreeSet<QpdfObjGen>,
    pub(crate) content_container_refs: BTreeSet<ObjectRef>,
    content_container_seq: HashMap<ObjectRef, u32>,
}

impl SpecialStreams {
    pub(crate) fn normalized_streams_raw(&self) -> &BTreeSet<QpdfObjGen> {
        &self.normalized_streams_raw
    }

    /// qpdf's `initializeSpecialStreams` builds `page_to_seq`/`object_to_seq`
    /// once, before either output route runs (`QPDFWriter.cc:1774-1781,
    /// 1914-1931`). The plain live QDF/normalize route's marker text
    /// (`%% Page N` / `%% Contents for page N`) and content-normalization
    /// membership gate must read this same setup-time snapshot rather than
    /// repeat the page walk at emission time, so a page-tree mutation
    /// between setup and body emission cannot make the two disagree.
    pub(crate) fn page_and_contents_sequences(
        &self,
    ) -> (BTreeMap<ObjectRef, usize>, BTreeMap<ObjectRef, usize>) {
        let to_btreemap = |map: &HashMap<ObjectRef, u32>| -> BTreeMap<ObjectRef, usize> {
            map.iter()
                .map(|(&object_ref, &sequence)| (object_ref, sequence as usize))
                .collect()
        };
        (to_btreemap(&self.page_seq), to_btreemap(&self.contents_seq))
    }
}

/// Build qpdf's page/content maps once for the writer setup trigger.
fn initialize_special_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
) -> Result<Option<SpecialStreams>> {
    if !(options.qdf || options.content_normalization || options.decode_level != DecodeLevel::None)
    {
        return Ok(None);
    }

    let pages = crate::PageDocumentHelper::new(pdf).get_all_pages()?;
    let mut streams = SpecialStreams {
        page_seq: HashMap::with_capacity(pages.len()),
        contents_seq: HashMap::new(),
        normalized_streams: BTreeSet::new(),
        normalized_streams_raw: BTreeSet::new(),
        content_container_refs: BTreeSet::new(),
        content_container_seq: HashMap::new(),
        pages,
    };

    for (index, page_ref) in streams.pages.iter().copied().enumerate() {
        let sequence = u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| Error::Internal("page sequence overflows u32".into()))?;
        streams.page_seq.insert(page_ref, sequence);
        for content_gen in collect_content_stream_qpdf_obj_gens(pdf, page_ref)? {
            if let Some(content_ref) = content_gen.to_object_ref() {
                streams.contents_seq.insert(content_ref, sequence);
                streams.normalized_streams.insert(content_ref);
            }
            streams.normalized_streams_raw.insert(content_gen);
        }
        if options.qdf || options.content_normalization {
            let mut content_containers = BTreeSet::new();
            collect_content_container_refs(pdf, page_ref, &mut content_containers)?;
            for container in content_containers {
                streams.content_container_refs.insert(container);
                streams.content_container_seq.insert(container, sequence);
            }
        }
    }

    Ok(Some(streams))
}

/// Return whether qpdf's content-derived ID branch is effective.
///
/// qpdf accepts both ID flags and checks `static_id` first in
/// `QPDFWriter::generateID` (`QPDFWriter.cc:1836-1878`). Keep the two input
/// settings independent while using this predicate wherever the writer must
/// choose between a static ID and the deterministic digest.
pub(crate) const fn uses_deterministic_id(options: &WriterOptions) -> bool {
    options.deterministic_id && !options.static_id
}

/// Translate qpdf's `QPDFWriter::prepareFileForWrite` graph preparation.
///
/// The qpdf writer calls `QPDF::fixDanglingReferences` and then makes a
/// dictionary-valued Catalog `/Extensions` direct, followed by an indirect
/// `/ADBE` child when present (`libqpdf/QPDFWriter.cc:2034-2055`). Every writer
/// consumer's ADBE add/remove decision happens in the root dictionary's
/// output-time `unparseObject` copy (`ObjectWriterEmission`).
///
/// This function is called once by [`PdfWriter::write`] before the standard or
/// linearized route is selected. The directization is permanent on the live
/// Catalog graph, matching qpdf; output-only ADBE reconciliation is owned by
/// each root serializer.
pub(crate) fn prepare_file_for_write<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    pdf.fix_dangling_references()?;

    let root = pdf.root_handle()?;
    let extensions = root.try_get_key(b"/Extensions")?;
    if !extensions.try_is_dictionary()? {
        return Ok(());
    }

    let extensions = if extensions.is_indirect() {
        let direct = extensions.shallow_copy()?;
        root.replace_key(b"/Extensions", direct.clone())?;
        direct
    } else {
        extensions
    };

    if extensions.try_has_key(b"/ADBE")? {
        let mut adbe = extensions.try_get_key(b"/ADBE")?;
        if adbe.is_indirect() {
            adbe.make_direct(false)?;
            extensions.replace_key(b"/ADBE", adbe)?;
        }
    }

    Ok(())
}

/// Configure qpdf-shaped progress after the writer has completed the setup
/// that allocates any synthetic objects. qpdf snapshots
/// `QPDF::getObjectCount()` only after `doWriteSetup` (QPDFWriter.cc:2189-2193),
/// so callers pass the number of fresh ObjStm containers allocated during that
/// setup without mutating the source document just for progress accounting.
pub(crate) fn configure_progress_for_pdf<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    additional_objects: usize,
    linearized: bool,
) -> Result<()> {
    if options.progress_reporter.is_none() {
        return Ok(());
    }

    // cov:ignore-start: Pdf::get_object_count returns u32, and flpdf's
    // supported targets have usize at least 32 bits wide.
    let object_count = usize::try_from(pdf.get_object_count()?).map_err(|_| {
        crate::Error::Unsupported("PdfWriter progress object count does not fit usize".into())
    })?;
    // cov:ignore-end
    configure_progress(
        options,
        object_count.saturating_add(additional_objects),
        linearized,
    );
    Ok(())
}

pub(crate) fn configure_progress(options: &WriterOptions, object_count: usize, linearized: bool) {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.configure(object_count.saturating_mul(if linearized { 2 } else { 1 }));
    } // cov:ignore: LLVM maps this closing brace as an executable branch line
}

pub(crate) fn report_progress_event(options: &WriterOptions) -> Result<()> {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.indicate(false, false)?;
    }
    Ok(())
}

pub(crate) fn decrement_progress_event(options: &WriterOptions) -> Result<()> {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.indicate(true, false)?;
    }
    Ok(())
}

pub(crate) fn report_progress_finished(options: &WriterOptions) -> Result<()> {
    if let Some(reporter) = options.progress_reporter.as_ref() {
        reporter.indicate(false, true)?;
    }
    Ok(())
}

/// True when `--force-version` pins the output header below PDF 1.5.
///
/// Object streams and cross-reference streams were both introduced in PDF 1.5.
/// qpdf treats a forced version as a hard cap it will not exceed, so when the
/// forced header is below 1.5 it suppresses those features entirely and falls
/// back to a classic xref table (observed on qpdf 11.9.0). `--min-version` is
/// only a floor — it never triggers this, because the 1.5 object-stream floor
/// raises above it — so this checks `force_version` specifically. Values whose
/// qpdf integer conversion overflows are rejected before this predicate is
/// reached by the CLI parser.
pub(crate) fn force_version_below_1_5(options: &WriterOptions) -> bool {
    options
        .force_version
        .as_deref()
        .filter(|version| !version.is_empty())
        .and_then(parse_qpdf_writer_version)
        .is_some_and(|version| version < QpdfVersionParts::new(1, 5))
}

/// Return the object-stream mode after qpdf's forced-version suppression.
///
/// `QPDFWriter::doWriteSetup` changes the writer's mode before route dispatch
/// when a forced version below 1.5 cannot represent object streams
/// (`QPDFWriter.cc:2103-2111`). Keep the coordinator's snapshot decision and
/// the inner emitter on that same effective mode so an output-only Catalog
/// mutation cannot bypass restoration.
fn effective_object_stream_mode(options: &WriterOptions) -> ObjectStreamMode {
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();
    if force_version_below_1_5(options)
        && (matches!(options.object_streams, ObjectStreamMode::Generate)
            || (!encrypting && matches!(options.object_streams, ObjectStreamMode::Preserve)))
    {
        ObjectStreamMode::Disable
    } else {
        options.object_streams
    }
}

/// Compute the effective PDF version to write given the source version and the
/// caller-supplied options.
///
/// Rule (mirrors qpdf):
/// 1. If `options.force_version` is set, use it verbatim.
/// 2. Otherwise start from `max(source, min_version_option)`.
/// 3. If `object_streams` is true, apply a `max(…, "1.5")` floor. Cross-
///    reference and object streams were introduced in PDF 1.5, so the output
///    must use at least 1.5 whenever such streams are actually emitted. The
///    caller passes whether the output *really* contains an object stream (not
///    merely whether the mode requests it), so a generate request that packs
///    nothing leaves the version untouched, matching qpdf.
///
/// If the version strings cannot be parsed the function falls back to the
/// `source` string unchanged (rather than panicking) so callers do not need to
/// validate before calling.
///
/// # `/Catalog /Version` reconciliation (qpdf semantics)
///
/// ISO 32000-1 §7.5.2 lets a `/Catalog /Version` entry override the header
/// when it is *higher*; readers compute the effective version as
/// `max(header, catalog)`. Empirically (verified against qpdf 11.x with
/// `qpdf --force-version` / `--min-version` on fixtures carrying a
/// `/Catalog /Version`), qpdf rewrites **only** the `%PDF-x.y` header line and
/// never strips, lowers, or otherwise touches `/Catalog /Version` — even when
/// it is higher than the chosen header. It also does **not** fold
/// `/Catalog /Version` into the source floor: the `--min-version` baseline is
/// the header version alone, not `max(header, catalog)`.
///
/// "Reconciled per qpdf semantics" therefore means *leave `/Catalog /Version`
/// alone* — `source` here is the header version and this function deliberately
/// does not read the Catalog. This keeps the implementation minimal and
/// byte-faithful to qpdf rather than guessing at a broader reconciliation.
pub(crate) fn effective_pdf_version<'a>(
    source: &'a str,
    options: &'a WriterOptions,
    object_streams: bool,
) -> &'a str {
    effective_pdf_version_and_ext(source, 0, options, object_streams).0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EffectivePdfVersion<'a> {
    raw: &'a str,
    parsed: QpdfVersionParts,
    extension_level: i64,
}

fn update_effective_pdf_version<'a>(
    current: &mut Option<EffectivePdfVersion<'a>>,
    raw: &'a str,
    extension_level: i64,
) {
    let Some(parsed) = parse_qpdf_writer_version(raw) else {
        return;
    };
    match current.as_mut() {
        None => {
            *current = Some(EffectivePdfVersion {
                raw,
                parsed,
                extension_level,
            });
        }
        Some(existing) => {
            if parsed > existing.parsed {
                // qpdf's setMinimumPDFVersion (QPDFWriter.cc:217-247) sets both
                // set_version and set_extension_level when the new numeric
                // version is strictly greater.
                existing.raw = raw;
                existing.parsed = parsed;
                existing.extension_level = extension_level;
            } else if parsed == existing.parsed && extension_level > existing.extension_level {
                // On a numeric tie, qpdf only sets set_extension_level; the
                // incumbent's raw version string is never replaced.
                existing.extension_level = extension_level;
            }
        }
    }
}

/// Header-version floor imposed by the encryption method requested via
/// [`WriterOptions::encrypt`] / [`WriterOptions::copy_encryption`].
///
/// Mirrors qpdf QPDFWriter.cc L806-815 (`setEncryptionParametersInternal`):
///
/// | Method                       | Floor (version, ext) |
/// |------------------------------|----------------------|
/// | V=5 R=6 AES-256              | (1.7, 8)             |
/// | V=5 R=5 AES-256 (legacy)     | (1.7, 3)             |
/// | V=4 R=4 AES-128              | (1.6, 0)             |
/// | V=4 R=4 RC4-128              | (1.5, 0)             |
/// | V=2 R=3 RC4-128              | (1.4, 0)             |
/// | V=1 R=2 RC4-40               | (1.3, 0)             |
/// | `copy_encryption`             | derived from copied V/R and AES mode |
///
/// qpdf's copy path forces AES for V>=4, so a copied V=4 source has the AESV2
/// floor even when the donor used RC4.
fn encryption_version_floor(options: &WriterOptions) -> Option<PdfVersion> {
    use crate::encryption::EncryptMethod;
    if let Some(ref enc) = options.encrypt {
        return Some(match enc.method {
            EncryptMethod::V5R6Aes256 => PdfVersion::new(1, 7, 8),
            EncryptMethod::V5R5Aes256 => PdfVersion::new(1, 7, 3),
            EncryptMethod::V4Aes128 => PdfVersion::new(1, 6, 0),
            EncryptMethod::V4Rc4128 => PdfVersion::new(1, 5, 0),
            EncryptMethod::V2Rc4128 => PdfVersion::new(1, 4, 0),
            EncryptMethod::V1Rc440 => PdfVersion::new(1, 3, 0),
        });
    }
    if let Some(source) = options.copy_encryption.as_ref() {
        let version = source
            .encrypt_dict
            .try_get_key(b"/V")
            .ok()?
            .try_as_integer()
            .ok()??;
        let revision = source
            .encrypt_dict
            .try_get_key(b"/R")
            .ok()?
            .try_as_integer()
            .ok()??;
        return Some(if revision >= 6 {
            PdfVersion::new(1, 7, 8)
        } else if revision == 5 {
            PdfVersion::new(1, 7, 3)
        } else if revision == 4 {
            // `copyEncryptionParameters` forces AES for every V>=4 donor
            // (`QPDFWriter.cc:674-679`), and
            // `setEncryptionParametersInternal` picks 1.6 for AES and 1.5
            // for RC4 at R=4 (`QPDFWriter.cc:806-814`).
            if version >= 4 {
                PdfVersion::new(1, 6, 0)
            } else {
                PdfVersion::new(1, 5, 0)
            }
        } else if revision == 3 {
            PdfVersion::new(1, 4, 0)
        } else {
            PdfVersion::new(1, 3, 0)
        });
    }
    None
}

/// Compute the effective (PDF version, Adobe extension level) pair to write,
/// applying qpdf's pairwise combined rule (`QPDFWriter::setMinimumPDFVersion`):
///
/// * `options.min_version` unset → take `(source, source_ext)`.
/// * new version > current → take both from the new source. The extension
///   level resets across a version bump; it does not carry across.
/// * new version == current AND new ext > current → take ext only.
/// * new version < current → ignore.
///
/// The extension level is only meaningful when greater than zero; callers
/// should injection-gate on that. `linearize` and `object_streams` are
/// threaded through unchanged.
fn forced_pdf_version_pair(options: &WriterOptions) -> Option<(&str, i64)> {
    let forced = options
        .force_version
        .as_deref()
        .filter(|version| !version.is_empty())?;
    parse_qpdf_writer_version(forced)?;
    Some((forced, options.force_extension_level.unwrap_or(0)))
}

pub(crate) fn effective_pdf_version_and_ext<'a>(
    source: &'a str,
    source_ext: i64,
    options: &'a WriterOptions,
    object_streams: bool,
) -> (&'a str, i64) {
    match forced_pdf_version_pair(options) {
        Some(pair) => pair,
        None => {
            effective_pdf_version_and_ext_without_force(source, source_ext, options, object_streams)
        }
    }
}

fn effective_pdf_version_and_ext_without_force<'a>(
    source: &'a str,
    source_ext: i64,
    options: &'a WriterOptions,
    object_streams: bool,
) -> (&'a str, i64) {
    // A PDF source header is normally strict M.m. Preserve the previous
    // defensive fallback for an overflowing source comparison value.
    if parse_qpdf_writer_version(source).is_none() {
        return (source, 0);
    }

    let mut best = None;
    // QPDFWriter's minimum is a pair. Keep the raw string and extension level
    // together so a strictly greater numeric version replaces both, while a
    // numeric tie raises only the extension level and keeps the incumbent raw
    // header spelling.
    if let Some(encryption_floor) = encryption_version_floor(options) {
        let raw = encryption_floor.static_version_str().unwrap_or("1.7");
        update_effective_pdf_version(&mut best, raw, encryption_floor.extension_level());
    }
    update_effective_pdf_version(&mut best, source, source_ext);
    if let Some(min_v) = options
        .min_version
        .as_deref()
        .filter(|version| !version.is_empty())
    {
        update_effective_pdf_version(&mut best, min_v, options.min_extension_level.unwrap_or(0));
    }
    if object_streams {
        update_effective_pdf_version(&mut best, "1.5", 0);
    }

    best.map(|version| (version.raw, version.extension_level))
        .unwrap_or((source, 0))
}

/// Apply the encryption floor after setup has moved the resolved parameters
/// out of `WriterOptions` and into the route state consumed by the live writer.
/// This preserves qpdf's V/R-dependent header and Adobe extension floor for
/// standard encrypted output, including V=5 R=5's `(1.7, 3)` pair.
pub(crate) fn effective_pdf_version_and_ext_with_encryption<'a>(
    source: &'a str,
    source_ext: i64,
    options: &'a WriterOptions,
    object_streams: bool,
    encryption: Option<&EncryptionParameters>,
) -> (&'a str, i64) {
    let (raw, extension_level) =
        effective_pdf_version_and_ext(source, source_ext, options, object_streams);
    let Some(encryption) = encryption else {
        return (raw, extension_level);
    };

    let (encryption_version, encryption_extension) = if encryption.encryption_r >= 6 {
        ("1.7", 8)
    } else if encryption.encryption_r == 5 {
        ("1.7", 3)
    } else if encryption.encryption_r == 4 {
        // `setEncryptionParametersInternal` keys R=4 on the AES/RC4 choice
        // (`libqpdf/QPDFWriter.cc:806-814`); the copy path forces AES for
        // every V>=4 donor (`libqpdf/QPDFWriter.cc:674-679`), while an
        // explicit V=4 RC4 method keeps 1.5.
        match encryption.cipher {
            WriteCipher::PerObject(crate::encryption::standard::ObjectKeyAlg::Rc4) => ("1.5", 0),
            WriteCipher::PerObject(crate::encryption::standard::ObjectKeyAlg::Aes)
            | WriteCipher::FileKeyAes256 => ("1.6", 0),
        }
    } else if encryption.encryption_r == 3 {
        ("1.4", 0)
    } else {
        ("1.3", 0)
    };

    let mut effective = None;
    update_effective_pdf_version(&mut effective, raw, extension_level);
    update_effective_pdf_version(&mut effective, encryption_version, encryption_extension);
    effective
        .map(|version| (version.raw, version.extension_level))
        .unwrap_or((raw, extension_level))
}

/// Binary header marker emitted by qpdf on the second line of every output
/// PDF (immediately after the `%PDF-x.y` version line).  The four bytes are
/// all > 127, which signals to file-transfer tools that the file is binary,
/// as recommended by the PDF specification.  We fix these to qpdf's values so
/// that flpdf output is byte-identical to qpdf output for the header section.
///
/// Hex: `25 BF F7 A2 FE 0A`  →  `%` + four high bytes + newline.
///
/// Shared with the linearization writer ([`crate::linearization`]) so the
/// linearized output uses the identical marker as the plain rewrite path.
pub(crate) const QPDF_BINARY_MARKER: &[u8] = b"%\xbf\xf7\xa2\xfe\n";

/// qpdf's PCLm version marker, written in place of the binary-comment marker
/// when PCLm output is selected (`QPDFWriter::writeHeader`,
/// `libqpdf/QPDFWriter.cc:2265-2275`).
pub(crate) const PCLM_HEADER_MARKER: &[u8] = b"%PCLm 1.0\n";

/// qpdf's static-id constant: the first 32 hex digits of π, encoded as 16 raw
/// bytes so the trailer emits `<31415926535897932384626433832795>`.
pub(crate) const QPDF_STATIC_ID: [u8; 16] = [
    0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93, 0x23, 0x84, 0x62, 0x64, 0x33, 0x83, 0x27, 0x95,
];

/// Generate a fresh 16-byte file identifier.
///
/// Mirrors qpdf's default-`/ID` algorithm in spirit: an MD5 digest seeded from
/// volatile per-invocation entropy (wall-clock nanoseconds, the process id, and
/// a strictly-monotonic process-global counter).  MD5 is already a direct
/// dependency, so no new crate is introduced.  The counter guarantees two calls
/// within the same nanosecond tick still produce distinct identifiers, which is
/// what makes "every save emits a different `/ID`" hold even for back-to-back
/// writes in a tight loop.
fn fresh_id_bytes() -> [u8; 16] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    let mut hasher = md5::Md5::new();
    use md5::Digest as _;
    hasher.update(nanos.to_le_bytes());
    hasher.update(pid.to_le_bytes());
    hasher.update(seq.to_le_bytes());
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// Full-rewrite path: decode+re-encode every stream
// ---------------------------------------------------------------------------

/// Write `pdf` as a full non-incremental rewrite.
///
/// Every stream is decoded through its filter chain and re-encoded with a
/// single `/FlateDecode` filter.  The output has no `/Prev` chain and no
/// `ObjStm` container objects.  ObjStm member objects are emitted as ordinary
/// indirect objects.  XRef stream container objects are replaced by a freshly
/// rebuilt xref table (or xref stream, matching the input's form).
///
/// # Metadata preservation policy
///
/// The `/Info` dictionary (containing `/Producer`, `/CreationDate`, `/ModDate`,
/// `/Author`, `/Title`, `/Creator`, `/Keywords`, `/Subject`, `/Trapped`, etc.)
/// is preserved **verbatim** from the source document.  No fields are added,
/// removed, or rewritten — in particular no "modified by flpdf" suffix is
/// appended to `/Producer`.  This mirrors `qpdf`'s default behaviour
/// (`qpdf in.pdf out.pdf`) and is required for byte-identical round-trip tests.
///
/// # Scope limitations
///
/// - **ObjStm dissolve**: Object streams are dissolved — members are emitted as
///   ordinary indirect objects.  There is currently no merging of existing
///   ObjStm containers back into the regular sequence; they are simply skipped.
///   A dedicated "renumber + pack into ObjStm" pass is not yet implemented.
///
/// - **Encrypted documents**: [`crate::PdfWriter`] follows qpdf and preserves
///   authenticated source encryption by default. Explicit
///   [`WriterOptions::encrypt`] or [`WriterOptions::copy_encryption`] settings
///   select a new encryption context.
///
/// Returns [`crate::Error::Missing`] if the input has no `/Root`.
mod _writer_doc_anchor {} // keeps the `emit_canonical_pdf` docstring above attached to its function.

// ── Encryption context ───────────────────────────────────────────

/// How the writer derives per-object string/stream encryption key material.
///
/// Mirrors the reader's per-object dispatch (`EncryptionMode`): V<5 handlers
/// derive a per-object key via Algorithm 1, while V=5 uses the 32-byte file
/// key directly with AES-256 (no per-object derivation).
#[derive(Debug, Clone, Copy)]
pub(crate) enum WriteCipher {
    /// V=1/V=2/V=4: per-object key via Algorithm 1, then RC4 or AES-128
    /// (the [`ObjectKeyAlg`](crate::encryption::standard::ObjectKeyAlg) selects
    /// the `sAlT` salt and the resulting cipher).
    PerObject(crate::encryption::standard::ObjectKeyAlg),
    /// V=5 R=5/R=6: the 32-byte file key is used directly with AES-256-CBC.
    /// There is no Algorithm-1 per-object derivation.
    FileKeyAes256,
}

/// Shared qpdf encryption parameter state before a route-specific `/Encrypt`
/// object number is allocated.
#[derive(Clone)]
pub(crate) struct EncryptionParameters {
    /// Built `/Encrypt` dictionary handle (from the Standard handler builder).
    pub(crate) encrypt_dict: ObjectHandle,
    /// File encryption key derived from passwords + `/ID[0]` (Algorithm 2),
    /// or — for V=5 — the random 32-byte file key (FEK).
    pub(crate) file_key: Vec<u8>,
    /// How per-object string/stream key material is derived (V<5 per-object
    /// vs V=5 file-key-direct).
    pub(crate) cipher: WriteCipher,
    /// Standard handler algorithm version (`/V`) used to derive writer data keys.
    pub(crate) encryption_v: i32,
    /// Standard handler revision (`/R`) retained with the writer encryption state.
    pub(crate) encryption_r: i32,
    /// The 16-byte `/ID[0]` bytes that were fed into the file-key derivation.
    /// The output trailer's `/ID` array MUST start with these same bytes —
    /// readers re-derive the file key from `/ID[0]` to validate the password.
    pub(crate) id0: Vec<u8>,
    /// When `true`, all AES CBC IVs are forced to `[0u8; 16]` instead of
    /// being drawn from the OS CSPRNG.  Testing only — mirrors
    /// [`WriterOptions::static_aes_iv`].
    pub(crate) static_aes_iv: bool,
    /// Whether the `/Metadata` stream is encrypted alongside the rest of the
    /// document (mirrors [`crate::EncryptParams::encrypt_metadata`]). When `false`
    /// (qpdf `--cleartext-metadata`, V=4/V=5 only), the `/Metadata` stream in
    /// [`metadata_ref`](Self::metadata_ref) is left in the clear instead of
    /// being run through the cipher.
    pub(crate) encrypt_metadata: bool,
    /// Indirect reference of the document `/Catalog`'s `/Metadata` stream, when
    /// one exists AND `encrypt_metadata` is `false`. Used by the emission loop
    /// to exempt exactly that object from encryption. `None` whenever metadata
    /// is encrypted (the common case) or the document has no `/Metadata`.
    pub(crate) metadata_ref: Option<ObjectRef>,
}

impl EncryptionParameters {
    pub(crate) fn into_context(self, encrypt_ref: ObjectRef) -> EncryptionContext {
        EncryptionContext {
            encrypt_dict: self.encrypt_dict,
            file_key: self.file_key,
            cipher: self.cipher,
            encryption_v: self.encryption_v,
            encryption_r: self.encryption_r,
            encrypt_ref,
            id0: self.id0,
            static_aes_iv: self.static_aes_iv,
            encrypt_metadata: self.encrypt_metadata,
            metadata_ref: self.metadata_ref,
        }
    }
}

/// Per-write encryption state after a route has assigned its `/Encrypt` slot.
pub(crate) struct EncryptionContext {
    pub(crate) encrypt_dict: ObjectHandle,
    pub(crate) file_key: Vec<u8>,
    pub(crate) cipher: WriteCipher,
    pub(crate) encryption_v: i32,
    pub(crate) encryption_r: i32,
    pub(crate) encrypt_ref: ObjectRef,
    pub(crate) id0: Vec<u8>,
    pub(crate) static_aes_iv: bool,
    pub(crate) encrypt_metadata: bool,
    pub(crate) metadata_ref: Option<ObjectRef>,
}

/// Resolve the document `/Catalog`'s `/Metadata` indirect reference, if any.
/// Used to exempt the XMP metadata stream from encryption under
/// `--cleartext-metadata`.
///
/// `pub(crate)`: also used by [`crate::linearization::writer::write_linearized_for_pdf_writer`],
/// which needs the same `--cleartext-metadata` exemption for linearized output.
///
/// # Errors
///
/// Propagates Catalog and `/Metadata` lookup failures; a missing or non-object
/// `/Metadata` value remains `Ok(None)`.
pub(crate) fn resolve_metadata_stream_ref<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Option<ObjectRef>> {
    let root_handle = pdf.root_handle()?;
    let metadata = root_handle.try_get_key(b"/Metadata")?;
    Ok(metadata.object_ref())
}

/// `id0` is the `/ID[0]` bytes the file encryption key is derived from
/// (PDF 1.7 §7.6.3.3 Algorithm 2); the caller must have already decided this
/// value — typically extracted from the writer's generated ID handle — and must write the
/// SAME bytes into the output trailer's `/ID[0]`, since a reader re-derives
/// the file key from `/ID[0]` to validate the password. Taking it as a
/// parameter (rather than resolving it internally from `pdf`) lets a caller
/// that already finalized `/ID` elsewhere (the linearized writer, which must
/// settle `/ID`'s final width before its two-pass probe loop runs) feed that
/// SAME value in, instead of this function re-deriving an independent one —
/// mirrors qpdf's own `generateID()`-is-idempotent contract: `/ID` is
/// computed once, and encryption setup consumes that single value
/// (`QPDFWriter::setEncryptionParameters` calls `generateID()` itself before
/// deriving `/O`/`/U`, and `writeTrailer`'s later call is a no-op).
pub(crate) fn build_encryption_parameters(
    options: &WriterOptions,
    params: &crate::encryption::EncryptParams,
    metadata_ref: Option<ObjectRef>,
    id0: &[u8],
) -> Result<EncryptionParameters> {
    use crate::encryption::standard::{
        build_v1_v2_encrypt_dict, build_v4_encrypt_dict, ObjectKeyAlg, V1V2EncryptParams,
        V4CryptMethod, V4EncryptParams,
    };
    use crate::encryption::EncryptMethod;

    let id0 = id0.to_vec();

    let (encrypt_dict, file_key, cipher, encryption_v, encryption_r) = match params.method {
        EncryptMethod::V4Aes128 => {
            let v4 = V4EncryptParams {
                method: V4CryptMethod::Aes,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
                encrypt_metadata: params.encrypt_metadata,
            };
            let (dict, key) = build_v4_encrypt_dict(&v4)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Aes), 4, 4)
        }
        EncryptMethod::V5R6Aes256 => {
            use crate::encryption::standard::{build_v5_r6_encrypt_dict, V5R6EncryptParams};
            // V=5 R=6 needs 68 bytes of fresh secret material (file key + four
            // 8-byte salts + 4-byte /Perms tail). Unlike V<5, /ID[0] does NOT
            // feed the key derivation — the file key is a standalone CSPRNG
            // value, so V=5 output is never byte-identical across runs.
            let secrets = generate_v5r6_secrets(options)?;
            let v5 = V5R6EncryptParams {
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                encrypt_metadata: params.encrypt_metadata,
            };
            let dict = build_v5_r6_encrypt_dict(&v5, &secrets)?;
            (
                dict,
                secrets.file_key.to_vec(),
                WriteCipher::FileKeyAes256,
                5,
                6,
            )
        }
        EncryptMethod::V5R5Aes256 => {
            use crate::encryption::standard::{build_v5_r5_encrypt_dict, V5R6EncryptParams};
            let secrets = generate_v5r6_secrets(options)?;
            let v5 = V5R6EncryptParams {
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                encrypt_metadata: params.encrypt_metadata,
            };
            let dict = build_v5_r5_encrypt_dict(&v5, &secrets)?;
            (
                dict,
                secrets.file_key.to_vec(),
                WriteCipher::FileKeyAes256,
                5,
                5,
            )
        }
        EncryptMethod::V1Rc440 => {
            // V=1 R=2 RC4-40. /EncryptMetadata is a V>=4 concept, so it is not
            // emitted here (V1V2EncryptParams has no such field).
            let v12 = V1V2EncryptParams {
                v: 1,
                r: 2,
                length_bits: 40,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.r2_permissions.to_p_bits(),
                id0: &id0,
            };
            let (dict, key) = build_v1_v2_encrypt_dict(&v12)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4), 1, 2)
        }
        EncryptMethod::V2Rc4128 => {
            // V=2 R=3 RC4-128 (qpdf's default for `--encrypt … 128`).
            let v12 = V1V2EncryptParams {
                v: 2,
                r: 3,
                length_bits: 128,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
            };
            let (dict, key) = build_v1_v2_encrypt_dict(&v12)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4), 2, 3)
        }
        EncryptMethod::V4Rc4128 => {
            // V=4 R=4 with /CFM V2 (RC4-128 crypt filter), e.g. `--force-V4`.
            let v4 = V4EncryptParams {
                method: V4CryptMethod::Rc4,
                user_password: &params.user_password,
                owner_password: &params.owner_password,
                p: params.permissions.to_p_bits(),
                id0: &id0,
                encrypt_metadata: params.encrypt_metadata,
            };
            let (dict, key) = build_v4_encrypt_dict(&v4)?;
            (dict, key, WriteCipher::PerObject(ObjectKeyAlg::Rc4), 4, 4)
        }
    };

    Ok(EncryptionParameters {
        encrypt_dict,
        file_key,
        cipher,
        encryption_v,
        encryption_r,
        id0,
        static_aes_iv: options.static_aes_iv,
        encrypt_metadata: params.encrypt_metadata,
        // Only exempt the /Metadata stream when cleartext metadata was actually
        // requested (the caller passes None when encrypt_metadata is true).
        metadata_ref: if params.encrypt_metadata {
            None
        } else {
            metadata_ref
        },
    })
}

/// Generate the fresh CSPRNG secret material V=5 R=6 encryption needs: the
/// 32-byte file key, four 8-byte password salts, and the 4-byte `/Perms`
/// tail. OS-RNG failure is surfaced as [`crate::Error::Unsupported`] rather
/// than panicking (mirrors the AES-IV generation in the stream pass).
fn generate_v5r6_secrets(
    _options: &WriterOptions,
) -> Result<crate::encryption::standard::V5R6Secrets> {
    #[cfg(any(test, feature = "qpdf-zlib-compat"))]
    if let Some(randomness) = _options.v5_randomness {
        return Ok(crate::encryption::standard::V5R6Secrets {
            file_key: randomness.file_key,
            user_validation_salt: randomness.user_validation_salt,
            user_key_salt: randomness.user_key_salt,
            owner_validation_salt: randomness.owner_validation_salt,
            owner_key_salt: randomness.owner_key_salt,
            perms_random_tail: randomness.perms_random_tail,
        });
    }

    let mut buf = [0u8; 68];
    getrandom::fill(&mut buf).map_err(|e| {
        crate::Error::Unsupported(format!(
            "OS CSPRNG (getrandom) unavailable for V=5 R=6 secret generation: {e}"
        ))
    })?;
    // Each range is a fixed, exact-length slice of `buf`, so the array
    // conversions are infallible by construction.
    Ok(crate::encryption::standard::V5R6Secrets {
        file_key: buf[0..32].try_into().unwrap(),
        user_validation_salt: buf[32..40].try_into().unwrap(),
        user_key_salt: buf[40..48].try_into().unwrap(),
        owner_validation_salt: buf[48..56].try_into().unwrap(),
        owner_key_salt: buf[56..64].try_into().unwrap(),
        perms_random_tail: buf[64..68].try_into().unwrap(),
    })
}

/// Build an [`EncryptionContext`] from a donor [`crate::CopyEncryptionSource`]
/// (the `--copy-encryption` path or PdfWriter's source-preservation
/// path).
///
/// qpdf does not copy the donor dictionary byte-for-byte. It passes the
/// authenticated donor values through `setEncryptionParametersInternal`:
/// V<4 remains RC4, V4 is always rewritten to AESV2, and V5 is rewritten to
/// AESV3 while retaining the donor's recovered file key. Rebuild the same
/// canonical dictionary here so a V4 RC4 donor has the same observable result
/// as qpdf's copy path.
pub(crate) fn build_copy_encryption_parameters(
    src: &crate::encryption::CopyEncryptionSource,
    options: &WriterOptions,
    metadata_ref: Option<ObjectRef>,
) -> Result<EncryptionParameters> {
    let (encrypt_dict, encryption_v, encryption_r, cipher, file_key) =
        canonical_copy_encryption(src)?;

    let encrypt_metadata = copy_encryption_encrypts_metadata_from_dict(&encrypt_dict);

    Ok(EncryptionParameters {
        encrypt_dict,
        file_key,
        cipher,
        encryption_v,
        encryption_r,
        id0: src.id0.clone(),
        static_aes_iv: options.static_aes_iv,
        encrypt_metadata,
        metadata_ref: if encrypt_metadata { None } else { metadata_ref },
    })
}

/// The immutable result of qpdf's one-time writer setup. Route consumers may
/// assign different `/Encrypt` object slots, but they must consume the same
/// parameter state and generated identifier material.
pub(crate) struct WriterSetupState {
    pub(crate) generated_id: Option<ObjectHandle>,
    pub(crate) encryption_parameters: Option<EncryptionParameters>,
    /// qpdf captures source ObjStm membership during `doWriteSetup`, before
    /// the later `getObjectCount` xref walk can reconstruct damaged input.
    pub(crate) source_object_stream_data: BTreeMap<u32, u32>,
    /// qpdf's Generate membership is computed before `prepareFileForWrite`;
    /// standard and linearized output consume this snapshot so preparation's
    /// directization cannot feed a stale post-prepare graph into the planner.
    pub(crate) generated_compressible: Option<object_streams::CompressiblePlan>,
    /// Fresh qpdf null placeholders allocated during setup for specialized
    /// Generate groups. Their source identities must survive into the live
    /// queue instead of being allocated after progress setup.
    pub(crate) generated_object_stream_sources: Vec<ObjectRef>,
}

/// Build the shared writer state before standard/linearized dispatch.
pub(crate) fn build_writer_setup<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
) -> Result<WriterSetupState> {
    let mut source_object_stream_data = BTreeMap::new();
    if options.object_streams == ObjectStreamMode::Preserve {
        pdf.get_object_stream_data(&mut source_object_stream_data);
    }
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();
    if uses_deterministic_id(options) && encrypting {
        return Err(deterministic_id_encryption_error(options));
    }

    let generated_id = if uses_deterministic_id(options) {
        None
    } else if let Some(source) = options.copy_encryption.as_ref() {
        let generated = generate_id_handle(None, options.static_id);
        let id1 = generated
            .as_array()
            .and_then(|values| values.get(1).and_then(ObjectHandle::as_string))
            .unwrap_or_else(|| source.id0.clone());
        Some(ObjectHandle::array(vec![
            ObjectHandle::string(source.id0.clone()),
            ObjectHandle::string(id1),
        ]))
    } else {
        let source_id0 = source_permanent_id_handle(&pdf.trailer())?;
        Some(generate_id_handle(source_id0.as_deref(), options.static_id))
    };

    let encryption_parameters = if let Some(params) = options.encrypt.as_ref() {
        let id0 = generated_id
            .as_ref()
            .and_then(ObjectHandle::as_array)
            .and_then(|values| values.first().and_then(ObjectHandle::as_string))
            .ok_or_else(|| {
                // cov:ignore-start: build_writer_setup always creates this two-string ID
                Error::Unsupported("writer setup: generated encryption ID is malformed".into())
                // cov:ignore-end
            })?; // cov:ignore: generated ID invariant makes this continuation unreachable
        let metadata_ref = if params.encrypt_metadata {
            None
        } else {
            resolve_metadata_stream_ref(pdf)?
        };
        Some(build_encryption_parameters(
            options,
            params,
            metadata_ref,
            &id0,
        )?) // cov:ignore: shared Standard builder is exercised; LLVM maps this continuation to setup
    } else if let Some(source) = options.copy_encryption.as_ref() {
        let metadata_ref = if copy_encryption_encrypts_metadata_from_dict(&source.encrypt_dict) {
            None
        } else {
            resolve_metadata_stream_ref(pdf)?
        };
        Some(build_copy_encryption_parameters(
            source,
            options,
            metadata_ref,
        )?)
    } else {
        None
    };

    Ok(WriterSetupState {
        generated_id,
        encryption_parameters,
        source_object_stream_data,
        generated_compressible: None,
        generated_object_stream_sources: Vec::new(),
    })
}

/// Rebuild the dictionary qpdf emits from `copyEncryptionParameters` and
/// select the corresponding object-key cipher.
fn canonical_copy_encryption(
    src: &crate::encryption::CopyEncryptionSource,
) -> Result<(ObjectHandle, i32, i32, WriteCipher, Vec<u8>)> {
    use crate::encryption::standard::ObjectKeyAlg;

    let version = copy_integer(&src.encrypt_dict, "V")?;
    let revision = copy_integer(&src.encrypt_dict, "R")?;
    let version_i32 = i32::try_from(version).map_err(|_| {
        crate::Error::Unsupported(format!("copy-encryption /V is out of range: {version}"))
    })?;
    let revision_i32 = i32::try_from(revision).map_err(|_| {
        crate::Error::Unsupported(format!("copy-encryption /R is out of range: {revision}"))
    })?;
    // qpdf reads key_len as 5 for V==1 and otherwise as the donor /Length
    // divided by eight, with no validation of either the value or the
    // handler it belongs to (`QPDFWriter.cc:667-670`).
    let length_bits = if version == 1 {
        40
    } else if let Some(length_bits) = src.writer_length_bits {
        length_bits
    } else {
        let key_len = copy_integer_as_int(&src.encrypt_dict, "Length")? / 8;
        key_len * 8
    };

    let p = crate::encryption::qpdf_permission_i32(copy_integer(&src.encrypt_dict, "P")?);
    let o = copy_string(&src.encrypt_dict, "O")?;
    let u = copy_string(&src.encrypt_dict, "U")?;
    let encrypt_metadata = copy_encryption_encrypts_metadata_from_dict(&src.encrypt_dict);

    // `setEncryptionParametersInternal` keeps the donor's authenticated key
    // for V>=5 and re-derives the V<5 key from the donor's padded user
    // password at the /Length-derived key length (`QPDFWriter.cc:832-839`).
    // Neither branch compares the result against the key length the reader
    // authenticated with, so a donor whose /Length disagrees with its real
    // key is copied rather than rejected.
    let file_key = if version >= 5 {
        src.file_key.clone()
    } else {
        let o_param = crate::encryption::standard::v_lt_5_32_byte_parameter(&o);
        let u_param = crate::encryption::standard::v_lt_5_32_byte_parameter(&u);
        crate::encryption::standard::compute_encryption_key_from_password(
            &src.padded_user_password,
            &crate::encryption::standard::StandardHandlerInputs {
                v: version,
                r: revision,
                length_bits,
                p,
                id0: &src.id0,
                u: &u_param,
                o: &o_param,
                encrypt_metadata,
            },
        )?
    };

    let mut entries = vec![
        (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
        (b"V".to_vec(), ObjectHandle::integer(version)),
        (b"Length".to_vec(), ObjectHandle::integer(length_bits)),
        (b"R".to_vec(), ObjectHandle::integer(revision)),
        (b"P".to_vec(), ObjectHandle::integer(i64::from(p))),
        (b"O".to_vec(), ObjectHandle::string(o)),
        (b"U".to_vec(), ObjectHandle::string(u)),
    ];

    let cipher = if version >= 5 {
        let oe = copy_string(&src.encrypt_dict, "OE")?;
        let ue = copy_string(&src.encrypt_dict, "UE")?;
        let perms = copy_string(&src.encrypt_dict, "Perms")?;
        entries.push((b"OE".to_vec(), ObjectHandle::string(oe)));
        entries.push((b"UE".to_vec(), ObjectHandle::string(ue)));
        entries.push((b"Perms".to_vec(), ObjectHandle::string(perms)));
        let (cf, stm_f, str_f) = standard_crypt_filter(b"AESV3", 32);
        entries.push((b"CF".to_vec(), cf));
        entries.push((b"StmF".to_vec(), stm_f));
        entries.push((b"StrF".to_vec(), str_f));
        WriteCipher::FileKeyAes256
    } else if version == 4 {
        // QPDFWriter::copyEncryptionParameters explicitly enables AES for all
        // V>=4 donors, even when the source /CFM was /V2.
        let (cf, stm_f, str_f) = standard_crypt_filter(b"AESV2", 16);
        entries.push((b"CF".to_vec(), cf));
        entries.push((b"StmF".to_vec(), stm_f));
        entries.push((b"StrF".to_vec(), str_f));
        WriteCipher::PerObject(ObjectKeyAlg::Aes)
    } else {
        WriteCipher::PerObject(ObjectKeyAlg::Rc4)
    };

    if revision >= 4 && !encrypt_metadata {
        entries.push((b"EncryptMetadata".to_vec(), ObjectHandle::boolean(false)));
    }
    let dict = ObjectHandle::dictionary(entries);
    Ok((dict, version_i32, revision_i32, cipher, file_key))
}

fn copy_integer(dict: &ObjectHandle, key: &str) -> Result<i64> {
    let key = format!("/{key}");
    dict.try_get_key(key.as_bytes())?
        .try_as_integer()?
        .ok_or_else(|| {
            crate::Error::Unsupported(format!("copy-encryption /{key} must be an integer"))
        })
}

/// Read an integer through qpdf's warning-and-zero fallback used by
/// `QPDFWriter::copyEncryptionParameters` for `/Length`.
fn copy_integer_as_int(dict: &ObjectHandle, key: &str) -> Result<i64> {
    let key = format!("/{key}");
    Ok(i64::from(
        dict.try_get_key(key.as_bytes())?
            .try_get_int_value_as_int()?,
    ))
}

fn copy_string(dict: &ObjectHandle, key: &str) -> Result<Vec<u8>> {
    let key = format!("/{key}");
    dict.try_get_key(key.as_bytes())?
        .as_string()
        .ok_or_else(|| {
            crate::Error::Unsupported(format!("copy-encryption /{key} must be a string"))
        })
}

fn standard_crypt_filter(cfm: &[u8], length: i64) -> (ObjectHandle, ObjectHandle, ObjectHandle) {
    let std_cf = ObjectHandle::dictionary(vec![
        (
            b"AuthEvent".to_vec(),
            ObjectHandle::name(b"DocOpen".to_vec()),
        ),
        (b"CFM".to_vec(), ObjectHandle::name(cfm.to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(length)),
    ]);
    (
        ObjectHandle::dictionary(vec![(b"StdCF".to_vec(), std_cf)]),
        ObjectHandle::name(b"StdCF".to_vec()),
        ObjectHandle::name(b"StdCF".to_vec()),
    )
}

/// Return the donor's metadata-encryption policy using qpdf's default. qpdf
/// only changes its default when `/EncryptMetadata` is present and boolean;
/// an absent or otherwise unusable entry means metadata remains encrypted.
pub(crate) fn copy_encryption_encrypts_metadata(
    src: &crate::encryption::CopyEncryptionSource,
) -> bool {
    copy_encryption_encrypts_metadata_from_dict(&src.encrypt_dict)
}

fn copy_encryption_encrypts_metadata_from_dict(dict: &ObjectHandle) -> bool {
    dict.try_get_key(b"/EncryptMetadata")
        .ok()
        .and_then(|value| value.as_boolean())
        .unwrap_or(true)
}

/// Append the lowercase-hex encoding of `bytes` to `out` via a table lookup,
/// avoiding the per-byte `String` allocation a `format!("{:02x}")` loop incurs.
/// Both the fixed-width `/ID` hex form and the deterministic-ID seed must be
/// lowercase hex byte-for-byte, which this matches.
fn push_hex_lower(out: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize]);
        out.push(HEX[(byte & 0x0f) as usize]);
    }
}

/// Extract the source trailer's non-empty `/ID[0]` through the live qpdf-shaped
/// handle graph for writer paths whose caller has already crossed the
/// `QPDF::getTrailer` boundary.
pub(crate) fn source_permanent_id_handle(trailer: &ObjectHandle) -> Result<Option<Vec<u8>>> {
    if !trailer.try_has_key(b"/ID")? {
        return Ok(None);
    }
    let id = trailer.try_get_key(b"/ID")?;
    source_permanent_id_value_handle(&id)
}

/// Extract qpdf's non-empty `/ID[0]` from an already-selected canonical value
/// handle. This is the one-value counterpart of `getTrailer().getKey("/ID")`.
pub(crate) fn source_permanent_id_value_handle(id: &ObjectHandle) -> Result<Option<Vec<u8>>> {
    let first = id.try_get_array_item(0)?;
    let value = first.try_get_string_value()?;
    Ok((!value.is_empty()).then_some(value))
}

/// Generate qpdf's ordinary/static two-element `/ID` array as a canonical
/// handle, preserving the same source-permanent-id and changing-id rules as
/// the writer's ordinary/static ID policy without crossing back through
/// `Object`.
pub(crate) fn generate_id_handle(source_id0: Option<&[u8]>, static_id: bool) -> ObjectHandle {
    let changing_id = if static_id {
        QPDF_STATIC_ID.to_vec()
    } else {
        fresh_id_bytes().to_vec()
    };
    let permanent_id = source_id0
        .filter(|id0| !id0.is_empty())
        .map(<[u8]>::to_vec)
        .unwrap_or_else(|| changing_id.clone());
    ObjectHandle::array(vec![
        ObjectHandle::string(permanent_id),
        ObjectHandle::string(changing_id),
    ])
}

/// Return qpdf's `QPDFWriter::generateID` logic error when deterministic ID
/// data is requested before the writer has emitted any bytes.
///
/// qpdf throws this exact message from `QPDFWriter.cc:1868-1874` when
/// `setEncryptionParameters` or `copyEncryptionParameters` reaches
/// `generateID` before the deterministic MD5 pipeline has produced its data.
pub(crate) fn generate_id_without_data() -> crate::Error {
    crate::Error::Internal(
        "INTERNAL ERROR: QPDFWriter::generateID has no data for deterministic ID.  This may happen if deterministic ID and file encryption are requested together."
            .to_string(),
    )
}

/// Return qpdf's `QPDFWriter::pushMD5Pipeline` logic error for a deterministic
/// digest enabled after the writer has already generated an `/ID`.
///
/// With `static_id`, `generateID` (`QPDFWriter.cc:1836`) succeeds during the
/// encryption setup and fills `id2`, so the failure moves to
/// `pushMD5Pipeline` (`QPDFWriter.cc:1011-1014`), which rejects a non-empty
/// `id2` with this exact message.
pub(crate) fn deterministic_id_after_id_generation() -> crate::Error {
    crate::Error::Internal(
        "Deterministic ID computation enabled after ID generation has already occurred."
            .to_string(),
    )
}

/// Select qpdf's logic error for deterministic ID mode combined with
/// encryption.
///
/// Without `static_id`, `generateID` fails first because the deterministic
/// digest has no data yet; with `static_id`, the encryption setup generates
/// the static `/ID` and `pushMD5Pipeline` fails afterwards instead.
pub(crate) fn deterministic_id_encryption_error(options: &WriterOptions) -> crate::Error {
    if options.static_id {
        deterministic_id_after_id_generation()
    } else {
        generate_id_without_data()
    }
}

/// Build the `/Info`-derived suffix of qpdf's deterministic `/ID` seed.
///
/// qpdf (`QPDFWriter::generateID`) appends, for every `/Info` entry whose value
/// is a string, `" "` followed by the string's *decoded* bytes, iterating keys
/// in sorted order (qpdf's `getKeys()` returns names sorted). Non-string
/// entries are skipped. The live `/Info` handle and each value may be an
/// indirect reference, so both are resolved (PDF allows any value to be
/// indirect, ISO 32000-1 §7.3.10). The returned bytes are appended after
/// `" QPDF "` to form the seed.
pub(crate) fn deterministic_id_info_suffix<R: Read + Seek>(pdf: &mut Pdf<R>) -> Vec<u8> {
    let trailer = pdf.trailer();
    let info = match trailer.try_get_key(b"/Info") {
        Ok(info) => info,
        Err(_) => return Vec::new(), // cov:ignore: a live-Pdf resolver failure is defensive; malformed source is rejected or normalized before this helper
    };
    let dict = match info.try_as_dictionary() {
        Ok(Some(dict)) => dict,
        Ok(None) => return Vec::new(),
        Err(_) => return Vec::new(), // cov:ignore: a live-Pdf resolver failure is defensive; malformed source is rejected or normalized before this helper
    };
    // `ObjectHandle::try_as_dictionary` returns qpdf's lexicographically sorted
    // decoded names, matching `QPDFObjectHandle::getKeys()`.
    let mut suffix = Vec::new();
    for (_key, value) in dict {
        if value.try_dereference().is_err() {
            continue; // cov:ignore: a live-Pdf resolver failure is defensive; malformed source is rejected or normalized before this helper
        }
        if let Some(bytes) = value.as_string() {
            suffix.push(b' ');
            suffix.extend_from_slice(&bytes);
        }
    }
    suffix
}

/// Compute qpdf's two-level deterministic `/ID` from a completed MD5 scope.
///
/// qpdf's linearized writer feeds the complete first-pass output through
/// `Pl_MD5`, then hashes the lowercase digest plus `" QPDF "` and the `/Info`
/// suffix. The suffix is treated as a C string for the second MD5, while the
/// first MD5 covers every accepted output byte. `/ID[0]` remains the source
/// permanent identifier when one exists, otherwise it copies `/ID[1]`.
pub(crate) fn compute_deterministic_id_from_digest(
    det_data: [u8; 16],
    info_suffix: &[u8],
    source_id0: Option<&[u8]>,
) -> (Vec<u8>, [u8; 16]) {
    use md5::Digest as _;

    // 32 hex chars for the 16-byte digest + " QPDF " (6) + the /Info suffix.
    let mut seed = Vec::with_capacity(32 + 6 + info_suffix.len());
    push_hex_lower(&mut seed, &det_data);
    seed.extend_from_slice(b" QPDF ");
    seed.extend_from_slice(info_suffix);
    // qpdf hashes the seed as a C string (`encodeString(seed.c_str())`), so it
    // stops at the first NUL. Mirror that strlen truncation; the leading hex
    // det_data and " QPDF " are NUL-free, so a NUL can only come from /Info.
    let seed_hash_input = &seed[..seed.iter().position(|&b| b == 0).unwrap_or(seed.len())];
    let id1: [u8; 16] = md5::Md5::digest(seed_hash_input).into();
    let id0 = source_id0
        .map(<[u8]>::to_vec)
        .unwrap_or_else(|| id1.to_vec());
    (id0, id1)
}

/// Direct-write qpdf's deterministic `/ID` array value INLINE at the current
/// output position, computing it from the bytes written so far.
///
/// Mirrors `QPDFWriter::generateID`: push `[`, MD5-digest the bytes written so
/// far (inclusive of the `[`), compute the two-level identifier, then write
/// `<id0_hex><id1_hex>]`. This
/// replaces the placeholder-then-byte-search scheme on the flat write paths, so
/// a crafted placeholder-shaped byte run elsewhere can never be mistaken for the
/// real `/ID`. The emitted bytes are identical to
/// the same computed identifier.
pub(crate) fn write_deterministic_id_inline(
    out: &mut OutputSink<'_>,
    info_suffix: &[u8],
    source_id0: Option<&[u8]>,
) -> Result<()> {
    use md5::Digest as _;

    out.write_bytes(b"[")?;
    out.suspend_digest();
    let output_digest = out.take_digest()?;
    let seed = output::deterministic_id_second_seed(&output_digest, info_suffix)?;
    let id1: [u8; 16] = md5::Md5::digest(&seed).into();
    let id0 = source_id0.unwrap_or(&id1);
    crate::pdf_syntax::write_hex_string(out, id0)?;
    crate::pdf_syntax::write_hex_string(out, &id1)?;
    out.write_bytes(b"]")
}

/// Apply writer-owned trailer values without converting the live trailer back
/// through the legacy `Dictionary` bridge. `/Root` and `/Encrypt` are already
/// output-space references, while `/ID` is a direct writer-owned array.
fn apply_encrypt_trailer_handle_entries<R: Read + Seek>(
    trailer: &ObjectHandle,
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    encrypt_ctx: Option<&EncryptionContext>,
    deterministic_id: bool,
    generated_id: Option<&ObjectHandle>,
) -> Result<()> {
    if let Some(ctx) = encrypt_ctx {
        trailer.replace_key(b"/Encrypt", pdf.get_object_handle(ctx.encrypt_ref))?; // cov:ignore: validated trailer mutation; LLVM attributes this continuation to the call setup
        if let Some(id) = generated_id {
            trailer.replace_key(b"/ID", id.shallow_copy()?)?;
        } else {
            let id1 = if options.static_id {
                QPDF_STATIC_ID.to_vec()
            } else {
                fresh_id_bytes().to_vec()
            };
            trailer.replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(ctx.id0.clone()),
                    ObjectHandle::string(id1),
                ]),
            )?; // cov:ignore: validated trailer mutation; LLVM attributes this continuation to the call setup
        }
    } else {
        if pdf.is_encrypted() {
            trailer.remove_key(b"/Encrypt");
        }
        if deterministic_id {
            trailer.replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(vec![0; 16]),
                    ObjectHandle::string(vec![0; 16]),
                ]),
            )?; // cov:ignore: validated deterministic-ID trailer mutation; LLVM attributes this continuation to the call setup
        } else if let Some(id) = generated_id {
            trailer.replace_key(b"/ID", id.shallow_copy()?)?;
        } else {
            // cov:ignore-start: generated_id is required before this non-encrypted trailer path
            return Err(crate::Error::Unsupported(
                "writer trailer is missing its generated /ID".to_string(),
            ));
            // cov:ignore-end
        }
    }
    Ok(())
}

/// Build the trimmed trailer shell used by the non-linearized full rewrite.
/// The source trailer and all surviving child values stay in the canonical
/// ObjectHandle graph; only writer-owned structural values are replaced.
#[allow(clippy::too_many_arguments)] // qpdf keeps source form, output form, ID, and encryption independent
fn build_writer_trailer_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    size: usize,
    root: Option<ObjectRef>,
    direct_root: Option<&ObjectHandle>,
    options: &WriterOptions,
    encrypt_ctx: Option<&EncryptionContext>,
    deterministic_id: bool,
    generated_id: Option<&ObjectHandle>,
) -> Result<ObjectHandle> {
    let trailer = pdf.trailer().unsafe_shallow_copy()?;
    for key in [b"/ID".as_slice(), b"/Encrypt", b"/Prev"] {
        trailer.remove_key(key);
    }
    // qpdf's getTrimmedTrailer removes every key that may describe an input
    // cross-reference stream, even when the input's active xref section is a
    // classic table with a hybrid /XRefStm link
    // (`libqpdf/QPDFWriter.cc:2009-2031`). These are writer-owned structural
    // values, not source trailer metadata to preserve.
    for key in [
        b"/Type".as_slice(),
        b"/W",
        b"/Index",
        b"/Length",
        b"/Filter",
        b"/DecodeParms",
        b"/XRefStm",
    ] {
        trailer.remove_key(key);
    }
    // qpdf's writeTrailer substitutes the computed size only for a literal
    // `/Size` key already present in the trimmed trailer. It does not repair a
    // missing or misspelled key on the normal writer path
    // (`QPDFWriter.cc:1174-1192`).
    if trailer.try_has_key(b"/Size")? {
        trailer.replace_key(
            b"/Size",
            ObjectHandle::integer(i64::try_from(size).map_err(|_| {
                // cov:ignore-start: supported writer object counts fit in i64
                crate::Error::Unsupported("writer trailer /Size does not fit in i64".to_string())
                // cov:ignore-end
            })?), // cov:ignore: supported writer object counts fit in i64
        )?; // cov:ignore: validated /Size replacement; LLVM attributes this continuation to the call setup
    }
    let root = match (root, direct_root) {
        (Some(root), None) => pdf.get_object_handle(root),
        (None, Some(root)) => root.clone(),
        _ => {
            return Err(crate::Error::Unsupported(
                "writer trailer Catalog root form is inconsistent".to_string(),
            ));
        }
    };
    trailer.replace_key(b"/Root", root)?; // cov:ignore: validated /Root replacement; LLVM attributes this continuation to the call setup
    apply_encrypt_trailer_handle_entries(
        &trailer,
        pdf,
        options,
        encrypt_ctx,
        deterministic_id,
        generated_id,
    )?; // cov:ignore: validated writer-owned trailer entries; LLVM attributes this continuation to the call setup
    Ok(trailer)
}

/// Translate a source ObjStm's `/Extends` target into the output container
/// number. qpdf resolves this relation through the source object-stream map;
/// only when the target is not itself preserved does it fall back to the
/// ordinary output renumber map (`QPDFWriter.cc:1731-1738`).
#[cfg(test)]
fn remap_source_objstm_extends(
    extends: ObjectRef,
    source_container_to_batch: &HashMap<ObjectRef, usize>,
    container_refs: &[ObjectRef],
    qdf: bool,
    qdf_emission_renumber: &HashMap<ObjectRef, ObjectRef>,
    renumber: &dyn crate::writer::rewrite_renumber::NewNumberLookup,
) -> Option<ObjectRef> {
    source_container_to_batch
        .get(&extends)
        .and_then(|batch_idx| container_refs.get(*batch_idx).copied())
        .or_else(|| {
            if qdf {
                qdf_emission_renumber.get(&extends).copied()
            } else {
                renumber.new_for_original(extends)
            }
        })
}

/// Whether `cipher` needs an AES CBC initialization vector: `true` for both
/// AES variants (V=4 AESV2 `PerObject(Aes)` and V=5 AESV3 `FileKeyAes256`),
/// `false` for RC4 (a stream cipher with no IV concept).
///
/// Shared by the canonical encrypted-string and stream pipeline stages and
/// `crate::linearization::writer::write_linearized` (which draws the hint
/// stream's single per-invocation IV under the same condition).
pub(crate) fn cipher_needs_aes_iv(cipher: WriteCipher) -> bool {
    use crate::encryption::standard::ObjectKeyAlg;
    matches!(
        cipher,
        WriteCipher::PerObject(ObjectKeyAlg::Aes) | WriteCipher::FileKeyAes256
    )
}

/// Apply qpdf's `QPDFWriter::adjustAESStreamLength` rule before a stream
/// dictionary is unparsed (`libqpdf/QPDFWriter.cc:965-973`).
fn writer_has_current_data_key(ctx: &EncryptionContext) -> bool {
    match ctx.cipher {
        WriteCipher::PerObject(_) => true,
        WriteCipher::FileKeyAes256 => !ctx.file_key.is_empty(),
    }
}

pub(crate) fn adjust_aes_stream_length(
    length: &mut usize,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
) -> Result<()> {
    if encrypt_stream && writer_has_current_data_key(ctx) && cipher_needs_aes_iv(ctx.cipher) {
        let padding = 32 - (*length & 0xf);
        *length = (*length).checked_add(padding).ok_or_else(|| {
            // cov:ignore-start: allocating a Vec large enough to overflow usize is infeasible.
            crate::Error::Unsupported("encrypted stream /Length overflows usize".to_string())
            // cov:ignore-end
        })?; // cov:ignore: llvm-cov attributes this continuation to the unreachable overflow arm.
    }
    Ok(())
}

/// Finish a writer pipeline even when its write phase fails. qpdf's
/// `PipelinePopper` calls `finish` from its destructor before it restores the
/// previous stack frame (`libqpdf/QPDFWriter.cc:925-963`).
fn run_writer_pipeline(pipeline: &mut dyn Pipeline, data: &[u8]) -> Result<()> {
    let write_result = pipeline.write(data);
    let finish_result = pipeline.finish();
    if let Err(error) = write_result {
        return Err(error.into());
    }
    finish_result.map_err(Into::into)
}

/// Pipeline tail that keeps encrypted stream bytes on the one counted final
/// output owner. Local filter/encryption `finish` calls terminate their own
/// stage chain; the enclosing writer invokes OutputSink's segment finish once
/// afterward so the configured target's exact error category is preserved.
struct OutputSinkPipeline<'output, 'sink> {
    out: &'output mut OutputSink<'sink>,
    failure: &'output mut Option<Error>,
}

impl Pipeline for OutputSinkPipeline<'_, '_> {
    fn identifier(&self) -> &str {
        "writer stream output"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        match self.out.write_bytes(data) {
            Ok(()) => Ok(()),
            Err(error) => {
                if self.failure.is_none() {
                    *self.failure = Some(error);
                }
                Err(PipelineError::runtime("writer output sink failed"))
            }
        }
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

/// Feed one emitted stream through qpdf's conditional encryption stage and
/// write it directly to the final output sink. The `Count` stage is qpdf's
/// base pipeline downstream of the optional encryption filter; QDF framing
/// observes the raw payload separately in [`write_stream_payload_with_pipeline_qdf`].
fn pipe_writer_stream_payload(
    out: &mut OutputSink<'_>,
    data: &[u8],
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<()> {
    let explicit_iv = explicit_iv.or_else(|| {
        (ctx.static_aes_iv && cipher_needs_aes_iv(ctx.cipher))
            .then(crate::pipeline::aes::static_initialization_vector)
    });
    let mut sink_failure = None;
    let pipeline_result = {
        let mut sink = OutputSinkPipeline {
            out,
            failure: &mut sink_failure,
        };
        let mut count = crate::pipeline::count::Count::new("writer stream count", &mut sink);
        if !encrypt_stream {
            run_writer_pipeline(&mut count, data)
        } else {
            let mut state = encryption_state::WriterEncryptionState::new(
                true,
                ctx.file_key.clone(),
                cipher_needs_aes_iv(ctx.cipher),
                ctx.encryption_v,
                ctx.encryption_r,
            );
            state.with_object_data_key(object_ref.number, None, |state| {
                let key = state.current_data_key().ok_or_else(|| {
                    // cov:ignore-start: with_object_data_key always sets the key before invoking the top-level callback, matching QPDFWriter::setDataKey.
                    crate::Error::Internal(
                        "QPDFWriter stream encryption data key was not initialized".to_string(),
                    )
                    // cov:ignore-end
                })?; // cov:ignore: LLVM maps the covered empty-key fallback pipeline continuation to this line
                if key.is_empty() {
                    run_writer_pipeline(&mut count, data)?;
                    return Ok(());
                }

                match ctx.cipher {
                    WriteCipher::PerObject(crate::encryption::standard::ObjectKeyAlg::Rc4) => {
                        let mut stage = crate::pipeline::rc4::PlRc4::new(
                            "rc4 stream encryption",
                            &mut count,
                            key,
                        )?; // cov:ignore: the preceding non-empty key guard makes the RC4 constructor's empty-key error unreachable here
                        run_writer_pipeline(&mut stage, data)
                    }
                    WriteCipher::PerObject(crate::encryption::standard::ObjectKeyAlg::Aes)
                    | WriteCipher::FileKeyAes256 => {
                        if let Some(iv) = explicit_iv {
                            count.write(&iv)?;
                            let mut stage = crate::pipeline::aes::PlAesPdf::new_encrypt(
                                "aes stream encryption",
                                &mut count,
                                key,
                            )?;
                            stage.set_iv(&iv)?;
                            run_writer_pipeline(&mut stage, data)
                        } else {
                            let mut stage = crate::pipeline::aes::PlAesPdf::new_encrypt(
                                "aes stream encryption",
                                &mut count,
                                key,
                            )?;
                            run_writer_pipeline(&mut stage, data)
                        }
                    }
                }
            })
        }
    };

    let finish_result = out.finish_segment();
    if let Some(error) = sink_failure {
        return Err(error);
    }
    pipeline_result?;
    finish_result
}

/// Write a stream payload through the qpdf-shaped writer pipeline, including
/// the final `endstream` framing decision based on the pipeline's last byte.
pub(crate) fn write_stream_payload_with_pipeline(
    out: &mut OutputSink<'_>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<bool> {
    write_stream_payload_with_pipeline_qdf(
        out,
        data,
        policy,
        false,
        object_ref,
        ctx,
        encrypt_stream,
        explicit_iv,
    )
}

/// Write an encrypted stream payload with qpdf's QDF-specific framing rule.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_stream_payload_with_pipeline_qdf(
    out: &mut OutputSink<'_>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
    qdf_mode: bool,
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<bool> {
    out.write_bytes(b"\nstream\n")?;
    pipe_writer_stream_payload(out, data, object_ref, ctx, encrypt_stream, explicit_iv)?;
    // qpdf's `m->pipeline` is the base Count stage downstream of the optional
    // encryption filter (`QPDFWriter.cc:976-995,1553-1558`), so its
    // `getLastChar()` observes the raw stream payload, not the final encrypted
    // byte. Keep the same QDF framing decision here.
    let last_byte = data.last().copied().unwrap_or(0);
    let add_newline = match policy {
        NewlineBeforeEndstream::Yes => true,
        NewlineBeforeEndstream::Never => qdf_mode && last_byte != b'\n',
    };
    if add_newline {
        out.write_bytes(b"\n")?;
    }
    out.write_bytes(b"endstream")?;
    Ok(add_newline)
}

#[cfg(test)]
pub(crate) fn emit_canonical_pdf<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    struct BorrowedWriterTarget<'a>(&'a mut dyn Write);

    impl OutputTarget for BorrowedWriterTarget<'_> {
        fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.write(bytes)
        }

        fn finish_segment(&mut self) -> Result<()> {
            self.0.flush().map_err(Error::Io)
        }

        // cov:ignore-start: this cfg(test) adapter emits bytes for unit tests; document finalization is owned by PdfWriter, not this helper.
        fn finish_document(&mut self) -> Result<()> {
            self.0.flush().map_err(Error::Io)
        }
        // cov:ignore-end
    }

    let setup = build_writer_setup(pdf, options)?;
    let special_streams = initialize_special_streams(pdf, options);
    let mut target = BorrowedWriterTarget(&mut out);
    let mut sink = OutputSink::new(&mut target);
    match special_streams {
        Ok(special_streams) => emit_canonical_pdf_with_special_streams(
            pdf,
            &mut sink,
            options,
            special_streams.as_ref(),
            setup,
        ),
        Err(error) => Err(error),
    }
}

fn emit_canonical_pdf_with_special_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    special_streams: Option<&SpecialStreams>,
    setup: WriterSetupState,
) -> Result<WriterResult> {
    emit_canonical_pdf_inner(pdf, out, options, special_streams, setup)
}

fn emit_canonical_pdf_inner<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    out: &mut OutputSink<'_>,
    options: &WriterOptions,
    special_streams: Option<&SpecialStreams>,
    setup: WriterSetupState,
) -> Result<WriterResult> {
    let WriterSetupState {
        generated_id,
        encryption_parameters,
        source_object_stream_data,
        generated_compressible,
        generated_object_stream_sources,
    } = setup;

    // qpdf suppresses every object-stream mode below a forced PDF 1.5
    // boundary. Normalize before entering the one live standard-writer queue
    // so no legacy fallback is needed merely to preserve the requested mode.
    let effective_object_streams = effective_object_stream_mode(options);
    let suppressed_options;
    let options = if effective_object_streams != options.object_streams {
        suppressed_options = WriterOptions {
            object_streams: effective_object_streams,
            ..options.clone()
        };
        &suppressed_options
    } else {
        options
    };

    if options.encrypt.is_some() && options.copy_encryption.is_some() {
        return Err(crate::Error::Unsupported(
            "encrypt and copy_encryption are mutually exclusive".to_string(),
        ));
    }
    // Every non-linearized standard route now has the same final OutputSink
    // and live queue owner. Encryption parameters remain writer setup state:
    // the body consumes their key/cipher policy while /Encrypt receives the
    // next output number only after the queue is exhausted.
    plain::write_plain(
        pdf,
        out,
        options,
        generated_id.as_ref(),
        encryption_parameters,
        &source_object_stream_data,
        special_streams,
        generated_compressible.as_ref(),
        &generated_object_stream_sources,
    )
}
fn collect_content_container_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    containers: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let page_handle = pdf.get_object_handle(page_ref);
    let contents = page_handle.try_get_key(b"/Contents")?;
    if contents.type_code()? == 10 {
        if contents.object_ref().is_none() {
            containers.insert(page_ref);
        }
        return Ok(());
    }

    if !contents.try_is_array()? {
        return Ok(());
    }
    containers.insert(contents.object_ref().unwrap_or(page_ref));
    Ok(())
}

/// Collect indirect page-content stream references through canonical
/// `ObjectHandle` inspection. Direct streams and malformed non-stream values
/// are omitted from the identity set: direct streams have no object identity
/// for `contents_seq`, while qpdf only normalizes actual stream objects.
///
/// This mirrors `QPDFWriter::initializeSpecialStreams`
/// (`libqpdf/QPDFWriter.cc:1914-1931`): resolve the page `/Contents` handle
/// once, inspect an array's immediate children, and never chase a
/// flpdf-only reference-holder chain. Unlike `ObjectHandle::get_page_contents`,
/// the writer pre-scan deliberately does not issue the `getPageContents`
/// damage warning for a non-stream array member, because qpdf's writer
/// pre-scan only asks each child whether it is a stream.
///
/// Production now reads the equivalent `ObjectRef` set from
/// [`SpecialStreams::page_and_contents_sequences`], computed once at setup;
/// this projection remains only for the `#[cfg(test)]`
/// `writer::plain::body::qdf_page_context` helper that re-derives the same
/// maps for unit tests calling `emit_live` directly.
#[cfg(test)]
pub(crate) fn collect_content_stream_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    Ok(collect_content_stream_qpdf_obj_gens(pdf, page_ref)?
        .into_iter()
        .filter_map(|object_gen| object_gen.to_object_ref())
        .collect())
}

/// Collect raw qpdf identities for page-content streams. Unlike the public
/// ObjectRef projection, this retains valid raw generations used by the
/// linearization normalization gate.
fn collect_content_stream_qpdf_obj_gens<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<QpdfObjGen>> {
    let page_handle = pdf.get_object_handle(page_ref);
    let contents = page_handle.try_get_key(b"/Contents")?;
    if contents.type_code()? == 10 {
        return Ok(contents
            .qpdf_obj_gen()
            .filter(|object_gen| object_gen.is_indirect())
            .into_iter()
            .collect());
    }

    let Some(items) = contents.try_as_array()? else {
        return Ok(Vec::new());
    };
    let mut refs = Vec::with_capacity(items.len());
    for item in items {
        if item.type_code()? == 10 {
            if let Some(object_gen) = item
                .qpdf_obj_gen()
                .filter(|object_gen| object_gen.is_indirect())
            {
                refs.push(object_gen);
            }
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod final_handle_writer_tests {
    use super::*;
    use crate::encryption::standard::ObjectKeyAlg;
    use crate::encryption::CopyEncryptionSource;
    use crate::pipeline::{PipelineError, PipelineResult};
    use crate::writer::object::TrailerKind;
    use std::io::{self, Cursor, Write};

    #[test]
    fn plain_route_consumes_the_prepared_generated_id() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let generated_id = ObjectHandle::array(vec![
            ObjectHandle::string(b"setup-id-0".to_vec()),
            ObjectHandle::string(b"setup-id-1".to_vec()),
        ]);
        let setup = WriterSetupState {
            generated_id: Some(generated_id),
            encryption_parameters: None,
            source_object_stream_data: BTreeMap::new(),
            generated_compressible: None,
            generated_object_stream_sources: Vec::new(),
        };
        let mut output = Vec::new();

        output::with_buffer_sink(&mut output, |out| {
            emit_canonical_pdf_inner(&mut pdf, out, &WriterOptions::default(), None, setup)
        })
        .expect("plain writer route succeeds");

        assert!(
            output
                .windows(b"<73657475702d69642d30>".len())
                .any(|window| window == b"<73657475702d69642d30>"),
            "plain route must emit the ID prepared by the shared writer setup"
        );
    }

    #[test]
    fn missing_source_id_emits_qpdf_copy_accessor_warning_chain() {
        let mut pdf = Pdf::empty().expect("empty PDF supplies a warning context");
        let id = pdf.trailer_key_handle(b"ID");
        let _ = source_permanent_id_value_handle(&id);
        let messages: Vec<_> = pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .map(|entry| String::from_utf8_lossy(entry.what_bytes()).into_owned())
            .collect();

        assert_eq!(
            messages.len(),
            2,
            "qpdf copy path must warn for both accessors"
        );
        assert!(messages[0]
            .contains("operation for array attempted on object of type null: returning null"));
        assert!(messages[1].contains(
            "null returned from invalid array access: operation for string attempted on object of type null: returning empty string"
        ));
    }

    #[test]
    fn generate_planning_rejects_a_missing_setup_snapshot() {
        // qpdf's `doWriteSetup` always runs `generateObjectStreams` before the
        // write route is chosen (`QPDFWriter.cc:2125-2139`), so a Generate
        // write without the setup-time membership is an internal invariant
        // violation rather than a cue to rewalk the prepared graph.
        let mut pdf = Pdf::open(std::io::Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .expect("open specialized Generate fixture");
        let setup = WriterSetupState {
            generated_id: Some(generate_id_handle(None, true)),
            encryption_parameters: None,
            source_object_stream_data: BTreeMap::new(),
            generated_compressible: None,
            generated_object_stream_sources: Vec::new(),
        };
        let options = WriterOptions {
            object_streams: ObjectStreamMode::Generate,
            preserve_unreferenced_objects: true,
            extra_header_text: "% specialized-live-queue\n".to_string(),
            static_id: true,
            ..WriterOptions::default()
        };
        let error = output::with_buffer_sink(&mut Vec::new(), |out| {
            emit_canonical_pdf_inner(&mut pdf, out, &options, None, setup)
        })
        .expect_err("Generate planning must require the setup snapshot");
        assert!(
            matches!(&error, Error::Internal(message)
                if message == "Generate object-stream planning requires the writer setup snapshot"),
            "unexpected error: {error}"
        );
    }

    struct AlwaysFailingOutput;

    impl Write for AlwaysFailingOutput {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("writer test output failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FinishFailingPipeline {
        writes: Rc<std::cell::Cell<usize>>,
        finishes: Rc<std::cell::Cell<usize>>,
    }

    impl Pipeline for FinishFailingPipeline {
        fn identifier(&self) -> &str {
            "writer test finish failure"
        }

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            self.finishes.set(self.finishes.get() + 1);
            Err(PipelineError::runtime(
                "writer stream segment finish failure",
            ))
        }
    }

    #[test]
    fn writer_pipeline_surfaces_a_segment_finish_failure_after_writing() {
        let writes = Rc::new(std::cell::Cell::new(0));
        let finishes = Rc::new(std::cell::Cell::new(0));
        let mut pipeline = FinishFailingPipeline {
            writes: Rc::clone(&writes),
            finishes: Rc::clone(&finishes),
        };
        assert_eq!(pipeline.identifier(), "writer test finish failure");

        let error = run_writer_pipeline(&mut pipeline, b"stream payload")
            .expect_err("a segment finish failure must escape the writer pipeline");
        assert!(error
            .to_string()
            .contains("writer stream segment finish failure"));
        assert_eq!(writes.get(), 1, "the segment payload was written once");
        assert_eq!(finishes.get(), 1, "the segment was finished once");
    }

    fn stream_encryption_context(
        cipher: WriteCipher,
        file_key: Vec<u8>,
        static_aes_iv: bool,
    ) -> EncryptionContext {
        EncryptionContext {
            encrypt_dict: ObjectHandle::dictionary(Vec::new()),
            file_key,
            cipher,
            encryption_v: match cipher {
                WriteCipher::FileKeyAes256 => 5,
                WriteCipher::PerObject(ObjectKeyAlg::Aes) => 4,
                WriteCipher::PerObject(ObjectKeyAlg::Rc4) => 2,
            },
            encryption_r: match cipher {
                WriteCipher::FileKeyAes256 => 6,
                WriteCipher::PerObject(ObjectKeyAlg::Aes) => 4,
                WriteCipher::PerObject(ObjectKeyAlg::Rc4) => 3,
            },
            encrypt_ref: ObjectRef::new(99, 0),
            id0: b"id".to_vec(),
            static_aes_iv,
            encrypt_metadata: true,
            metadata_ref: None,
        }
    }

    #[test]
    fn stream_pipeline_covers_plain_empty_key_rc4_and_aes_stage_routes() {
        let data = b"stream payload";

        let mut plain = Vec::new();
        output::with_buffer_sink(&mut plain, |out| {
            pipe_writer_stream_payload(
                out,
                data,
                ObjectRef::new(3, 0),
                &stream_encryption_context(
                    WriteCipher::PerObject(ObjectKeyAlg::Rc4),
                    Vec::new(),
                    true,
                ),
                false,
                None,
            )
        })
        .expect("disabled stream encryption leaves payload plain");
        assert_eq!(plain, data);

        let mut empty_file_key = Vec::new();
        output::with_buffer_sink(&mut empty_file_key, |out| {
            pipe_writer_stream_payload(
                out,
                data,
                ObjectRef::new(3, 0),
                &stream_encryption_context(WriteCipher::FileKeyAes256, Vec::new(), true),
                true,
                None,
            )
        })
        .expect("an empty V5 file key uses the cleartext pipeline fallback");
        assert_eq!(empty_file_key, data);

        for (static_aes_iv, explicit_iv) in [(true, Some([4; 16])), (false, None)] {
            let mut short_aes = Vec::new();
            let error = output::with_buffer_sink(&mut short_aes, |out| {
                pipe_writer_stream_payload(
                    out,
                    data,
                    ObjectRef::new(3, 0),
                    &stream_encryption_context(
                        WriteCipher::FileKeyAes256,
                        vec![1; 15],
                        static_aes_iv,
                    ),
                    true,
                    explicit_iv,
                )
            })
            .expect_err("a short AES key must be rejected by the encryption pipeline");
            assert!(error.to_string().contains("at least 16"));
        }

        let mut rc4 = Vec::new();
        output::with_buffer_sink(&mut rc4, |out| {
            pipe_writer_stream_payload(
                out,
                data,
                ObjectRef::new(3, 0),
                &stream_encryption_context(
                    WriteCipher::PerObject(ObjectKeyAlg::Rc4),
                    vec![1; 5],
                    true,
                ),
                true,
                None,
            )
        })
        .expect("RC4 stage writes ciphertext");
        assert_eq!(rc4.len(), data.len());
        assert_ne!(rc4, data);

        for explicit_iv in [Some([2; 16]), None] {
            let mut aes = Vec::new();
            output::with_buffer_sink(&mut aes, |out| {
                pipe_writer_stream_payload(
                    out,
                    data,
                    ObjectRef::new(3, 0),
                    &stream_encryption_context(
                        WriteCipher::PerObject(ObjectKeyAlg::Aes),
                        vec![1; 16],
                        explicit_iv.is_some(),
                    ),
                    true,
                    explicit_iv,
                )
            })
            .expect("AES stage writes IV-prefixed ciphertext");
            assert!(aes.len() >= 32);
            assert_ne!(&aes[..data.len()], data);
        }

        let mut aes256 = Vec::new();
        output::with_buffer_sink(&mut aes256, |out| {
            pipe_writer_stream_payload(
                out,
                data,
                ObjectRef::new(3, 0),
                &stream_encryption_context(WriteCipher::FileKeyAes256, vec![1; 32], true),
                true,
                Some([3; 16]),
            )
        })
        .expect("AES-256 stage writes IV-prefixed ciphertext");
        assert!(aes256.len() >= 32);
        assert_ne!(&aes256[..data.len()], data);
    }

    #[test]
    fn output_sink_pipeline_reports_its_stage_identity() {
        let mut bytes = Vec::new();
        let mut out = OutputSink::new(&mut bytes);
        let mut failure = None;
        let mut pipeline = OutputSinkPipeline {
            out: &mut out,
            failure: &mut failure,
        };

        assert_eq!(pipeline.identifier(), "writer stream output");
        pipeline
            .finish()
            .expect("sink tail has no nested finish work");
    }

    fn shared_trailer_contract_fixture(
    ) -> (Pdf<Cursor<Vec<u8>>>, ObjectHandle, ObjectRef, ObjectRef) {
        let pdf = Pdf::empty().expect("empty PDF for trailer fixture");
        let root = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(2))
            .expect("indirect root fixture");
        let encrypt = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .expect("indirect encryption fixture");
        let root_ref = root.object_ref().expect("root object reference");
        let encrypt_ref = encrypt.object_ref().expect("encryption object reference");
        let trailer = ObjectHandle::dictionary(vec![
            (b"/Info".to_vec(), ObjectHandle::integer(1)),
            (b"/Name".to_vec(), ObjectHandle::name(b"N".to_vec())),
            (b"/CustomRef".to_vec(), root.clone()),
            (b"/Root".to_vec(), root),
            (b"/Size".to_vec(), ObjectHandle::integer(99)),
            (b"/NullEntry".to_vec(), ObjectHandle::null()),
            (
                b"/ID".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::string(b"id0".to_vec()),
                    ObjectHandle::string(b"id1".to_vec()),
                ]),
            ),
            (b"/Encrypt".to_vec(), encrypt),
        ]);
        (pdf, trailer, root_ref, encrypt_ref)
    }

    #[test]
    fn shared_trailer_contract_emits_normal_qdf_and_linearized_forms() {
        let (_pdf, trailer, root_ref, encrypt_ref) = shared_trailer_contract_fixture();
        let map = |object_ref| Ok(object_ref);
        let removed = BTreeSet::new();

        let mut normal = Vec::new();
        output::with_buffer_sink(&mut normal, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 6 },
                false,
                false,
                None,
                &map,
                &removed,
                true,
            )
        })
        .expect("normal shared trailer succeeds");
        assert_eq!(
            normal,
            format!(
                "trailer << /CustomRef {} 0 R /Info 1 /Name /N /Root {} 0 R /Size 6 /ID [<696430><696431>] /Encrypt {} 0 R >>",
                root_ref.number, root_ref.number, encrypt_ref.number
            )
            .as_bytes()
        );

        let mut qdf = Vec::new();
        output::with_buffer_sink(&mut qdf, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 6 },
                false,
                true,
                None,
                &map,
                &removed,
                true,
            )
        })
        .expect("QDF shared trailer succeeds");
        assert_eq!(
            qdf,
            format!(
                "trailer <<\n  /CustomRef {} 0 R\n  /Info 1\n  /Name /N\n  /Root {} 0 R\n  /Size 6\n  /ID [<696430><696431>] /Encrypt {} 0 R\n>>\n",
                root_ref.number, root_ref.number, encrypt_ref.number
            )
            .as_bytes()
        );

        let mut first = Vec::new();
        output::with_buffer_sink(&mut first, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::LinearizedFirst { size: 6, prev: 123 },
                false,
                false,
                None,
                &map,
                &removed,
                true,
            )
        })
        .expect("linearized first trailer succeeds");
        let first_text = String::from_utf8(first).expect("trailer is UTF-8");
        let prev_start = first_text.find("/Prev ").expect("/Prev is present") + 6;
        assert_eq!(
            &first_text.as_bytes()[prev_start..prev_start + 21],
            b"123                  "
        );
        assert!(first_text.ends_with(&format!(
            " /ID [<696430><696431>] /Encrypt {} 0 R >>",
            encrypt_ref.number
        )));

        let mut second = b"<< /Type /XRef".to_vec();
        output::with_buffer_sink(&mut second, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::LinearizedSecond { size: 6 },
                true,
                false,
                None,
                &map,
                &removed,
                true,
            )
        })
        .expect("linearized second xref trailer succeeds");
        assert_eq!(second, b"<< /Type /XRef /Size 6 /ID [<696430><696431>] >>");
    }

    /// `xref_stream=true` combined with `qdf=true` is qpdf's shape for a QDF
    /// file whose writer generated (or preserved) object streams: qpdf's
    /// `writeXRefStream` writes the fixed dictionary prefix itself, then
    /// calls the *same* `writeTrailer(which, size, xref_stream=true, ...)`
    /// used by the classic table route (`QPDFWriter.cc:2470,1159-1236`).
    /// `xref_stream=true` skips the `trailer <<` keyword in both modes;
    /// `qdf` only ever controls whether a `\n` separates it from the caller's
    /// prefix (`writeStringQDF("\n")` runs unconditionally after the
    /// `if (xref_stream) {...} else {writeString("trailer <<");}` branch).
    /// This is the cell no production caller exercised before this owner
    /// became the xref-stream-embedded route's single trailer serializer.
    #[test]
    fn shared_trailer_contract_qdf_xref_stream_omits_trailer_keyword_but_keeps_newline() {
        let (_pdf, trailer, root_ref, encrypt_ref) = shared_trailer_contract_fixture();
        let map = |object_ref| Ok(object_ref);
        let removed = BTreeSet::new();

        let mut output = b"<< /Type /XRef".to_vec();
        output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 6 },
                true,
                true,
                None,
                &map,
                &removed,
                true,
            )
        })
        .expect("QDF xref-stream-embedded trailer succeeds");
        assert_eq!(
            output,
            format!(
                "<< /Type /XRef\n  /CustomRef {} 0 R\n  /Info 1\n  /Name /N\n  /Root {} 0 R\n  /Size 6\n  /ID [<696430><696431>] /Encrypt {} 0 R\n>>\n",
                root_ref.number, root_ref.number, encrypt_ref.number
            )
            .as_bytes()
        );
    }

    #[test]
    fn linearized_second_trailer_synthesizes_a_missing_size_like_qpdf() {
        let (_pdf, trailer, _root_ref, _encrypt_ref) = shared_trailer_contract_fixture();
        trailer.remove_key(b"/Size");

        let mut output = Vec::new();
        output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::LinearizedSecond { size: 9 },
                false,
                false,
                None,
                &|object_ref| Ok(object_ref), // cov:ignore: direct /ID strings need no reference map
                &BTreeSet::new(),
                true,
            )
        })
        .expect("linearized second trailer succeeds without source /Size");

        assert_eq!(output, b"trailer << /Size 9 /ID [<696430><696431>] >>");

        let mut qdf_output = Vec::new();
        output::with_buffer_sink(&mut qdf_output, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::LinearizedSecond { size: 9 },
                false,
                true,
                None,
                &|object_ref| Ok(object_ref), // cov:ignore: direct /ID strings need no reference map
                &BTreeSet::new(),
                true,
            )
        })
        .expect("QDF linearized second trailer succeeds without source /Size");
        assert_eq!(
            qdf_output,
            b"trailer <<\n  /Size 9\n  /ID [<696430><696431>]\n>>\n"
        );
    }

    #[test]
    fn shared_trailer_contract_preserves_writer_owned_keys_and_id_writer() {
        let (_pdf, trailer, root_ref, encrypt_ref) = shared_trailer_contract_fixture();
        let map = |object_ref: ObjectRef| -> Result<ObjectRef> {
            Ok(ObjectRef::new(object_ref.number + 100, 0))
        };
        let removed = BTreeSet::new();
        let mut output = Vec::new();
        let mut id_writer = |out: &mut OutputSink<'_>| out.write_bytes(b"[<custom>]");

        output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 7 },
                false,
                false,
                Some(&mut id_writer),
                &map,
                &removed,
                true,
            )
        })
        .expect("writer-owned trailer values survive filtering");
        let text = String::from_utf8(output).expect("trailer is UTF-8");
        assert!(!text.contains("/NullEntry"));
        assert!(text.contains(&format!("/Root {} 0 R", root_ref.number)));
        assert!(text.contains(&format!(
            "/ID [<custom>] /Encrypt {} 0 R",
            encrypt_ref.number
        )));
    }

    #[test]
    fn shared_trailer_contract_rejects_reserved_and_non_dictionary_handles() {
        let reserved = ObjectHandle::new_reserved_direct();
        let error = output::with_buffer_sink(&mut Vec::new(), |out| {
            reserved.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 1 },
                false,
                false,
                None,
                &|object_ref| Ok(object_ref), // cov:ignore: reserved handles exit before the callback can run
                &BTreeSet::new(),
                true,
            )
        })
        .expect_err("reserved trailer handle must fail");
        assert!(matches!(error, Error::System(message) if message.contains("reserved")));

        let scalar = ObjectHandle::integer(1);
        let mut output = Vec::new();
        output::with_buffer_sink(&mut output, |out| {
            scalar.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 1 },
                false,
                false,
                None,
                &|object_ref| Ok(object_ref), // cov:ignore: scalar handles have no child reference to map
                &BTreeSet::new(),
                true,
            )
        })
        .expect("non-dictionary trailer is emitted as an empty shell");
        assert_eq!(output, b"trailer << >>");
    }

    #[test]
    fn shared_trailer_contract_maps_a_direct_qdf_root() {
        let trailer = ObjectHandle::dictionary(vec![
            (
                b"/Root".to_vec(),
                ObjectHandle::dictionary(vec![(b"/Pages".to_vec(), ObjectHandle::integer(3))]),
            ),
            (b"/Size".to_vec(), ObjectHandle::integer(1)),
        ]);
        // cov:ignore-start: this direct-root case has no indirect child to map
        let map = |object_ref: ObjectRef| -> Result<ObjectRef> {
            Ok(ObjectRef::new(object_ref.number + 100, 0))
        };
        // cov:ignore-end
        let mut output = Vec::new();
        output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 1 },
                false,
                true,
                None,
                &map,
                &BTreeSet::new(),
                true,
            )
        })
        .expect("direct QDF Catalog is emitted");
        let text = String::from_utf8(output).expect("trailer is UTF-8");
        assert!(text.contains("/Pages 3"));
    }

    #[test]
    fn shared_trailer_contract_filters_a_removed_reference() {
        let pdf = Pdf::empty().expect("empty PDF for removed-reference test");
        let custom = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(3))
            .expect("indirect custom trailer value");
        let custom_ref = custom.object_ref().expect("custom object reference");
        let trailer = ObjectHandle::dictionary(vec![
            (b"/CustomRef".to_vec(), custom),
            (b"/Size".to_vec(), ObjectHandle::integer(2)),
        ]);
        let mut output = Vec::new();
        output::with_buffer_sink(&mut output, |out| {
            trailer.write_trailer_with_ref_map_and_kind(
                out,
                TrailerKind::Normal { size: 2 },
                false,
                false,
                None,
                &|object_ref| Ok(object_ref), // cov:ignore: removed references exit before the callback can run
                &[custom_ref].into_iter().collect(),
                true,
            )
        })
        .expect("removed trailer reference is filtered");
        assert!(!String::from_utf8(output)
            .expect("trailer is UTF-8")
            .contains("/CustomRef"));
    }

    #[test]
    fn prepare_file_for_write_fixes_dangling_cache_before_catalog_directization() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-ext-indirect.pdf").to_vec(),
        ))
        .expect("indirect Extensions fixture opens");
        assert!(!pdf.dangling_references_fixed());

        prepare_file_for_write(&mut pdf).expect("writer preparation succeeds");

        assert!(
            pdf.dangling_references_fixed(),
            "fixDanglingReferences must resolve the source xref before Catalog work"
        );
        let root = pdf.root_handle().expect("Catalog remains available");
        assert!(
            root.try_get_key(b"/Extensions")
                .expect("read prepared Extensions")
                .is_direct(),
            "an indirect Extensions dictionary must be direct on the live Catalog"
        );
    }

    #[test]
    fn prepare_file_for_write_directizes_an_indirect_adbe_child() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-ext-indirect.pdf").to_vec(),
        ))
        .expect("indirect Extensions fixture opens");
        let root = pdf.root_handle().expect("Catalog exists");
        let extensions = root
            .try_get_key(b"/Extensions")
            .expect("read source Extensions");
        let adbe = extensions.try_get_key(b"/ADBE").expect("read source ADBE");
        let indirect_adbe = pdf
            .make_indirect_from_object_handle(adbe)
            .expect("promote ADBE to an indirect object");
        extensions
            .replace_key(b"/ADBE", indirect_adbe)
            .expect("install indirect ADBE");

        prepare_file_for_write(&mut pdf).expect("writer preparation succeeds");

        let root = pdf.root_handle().expect("Catalog remains available");
        let extensions = root
            .try_get_key(b"/Extensions")
            .expect("read prepared Extensions");
        assert!(extensions.is_direct());
        assert!(
            extensions
                .try_get_key(b"/ADBE")
                .expect("read prepared ADBE")
                .is_direct(),
            "an indirect ADBE value must be made direct by prepareFileForWrite"
        );
    }

    #[test]
    fn prepare_file_for_write_survives_a_later_emission_failure() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-ext-indirect.pdf").to_vec(),
        ))
        .expect("indirect Extensions fixture opens");
        let mut writer = PdfWriter::new(&mut pdf);
        let mut output = AlwaysFailingOutput;
        assert!(output.flush().is_ok());
        writer
            .set_output_writer(output)
            .expect("install failing output");

        assert!(writer.write().is_err(), "the output sink must fail");

        let root = pdf.root_handle().expect("Catalog remains available");
        assert!(
            root.try_get_key(b"/Extensions")
                .expect("read prepared Extensions after failure")
                .is_direct(),
            "a later write failure must not restore the pre-prepare indirect graph"
        );
    }

    #[test]
    fn minimum_version_replaces_an_unusable_existing_internal_value() {
        let mut current = Some(("2147483648".to_string(), 0));

        update_minimum_pdf_version(&mut current, "1.7".to_string(), 0);

        assert_eq!(current, Some(("1.7".to_string(), 0)));

        let mut current = Some(("1.4".to_string(), 0));
        update_minimum_pdf_version(&mut current, "1.7".to_string(), 0);

        assert_eq!(current, Some(("1.7".to_string(), 0)));

        let mut current = Some(("1.7".to_string(), 1));
        update_minimum_pdf_version(&mut current, "1.7".to_string(), 2);

        assert_eq!(current, Some(("1.7".to_string(), 2)));
    }

    #[test]
    fn minimum_version_numeric_tie_keeps_incumbent_raw_version() {
        let mut current = Some(("1.7".to_string(), 0));

        update_minimum_pdf_version(&mut current, "1.7x".to_string(), 2);

        assert_eq!(current, Some(("1.7".to_string(), 2)));
    }

    #[test]
    fn forced_raw_version_uses_qpdf_encryption_compatibility_floors() {
        for (params, forced_version) in [
            (
                EncryptParams::rc4(EncryptMethod::V1Rc440, b"u", b"o"),
                "1.2",
            ),
            (
                EncryptParams::rc4(EncryptMethod::V2Rc4128, b"u", b"o"),
                "1.3",
            ),
            (
                EncryptParams::rc4(EncryptMethod::V4Rc4128, b"u", b"o"),
                "1.4",
            ),
            (EncryptParams::v4_aes128(b"u", b"o"), "1.5"),
            (EncryptParams::v5_r6(b"u", b"o"), "1.7"),
        ] {
            let options = WriterOptions {
                encrypt: Some(params),
                force_version: Some(forced_version.to_string()),
                ..WriterOptions::default()
            };
            assert!(
                forced_version_disables_encryption(&options),
                "forced version {forced_version} must disable its incompatible encryption"
            );
        }
    }

    #[test]
    fn malformed_v4_aes256_parameters_use_the_r4_aes_floor() {
        // `setEncryptionParametersInternal` keys the floor on `/R`
        // (`libqpdf/QPDFWriter.cc:806-814`): R=4 picks 1.6 for AES (which
        // includes a malformed 256-bit key) and 1.5 for RC4. The 1.7/3
        // extension floor belongs to R=5 only.
        let encryption = EncryptionParameters {
            encrypt_dict: ObjectHandle::dictionary(Vec::new()),
            file_key: vec![1; 32],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 4,
            encryption_r: 4,
            id0: b"id".to_vec(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };

        assert_eq!(
            effective_pdf_version_and_ext_with_encryption(
                "1.4",
                0,
                &WriterOptions::default(),
                false,
                Some(&encryption),
            ),
            ("1.6", 0)
        );
    }

    #[test]
    fn encryption_version_floor_is_keyed_on_revision_like_qpdf() {
        // `setEncryptionParametersInternal` picks the floor from `/R`
        // (`libqpdf/QPDFWriter.cc:806-814`), so the V<5 / R>3 cells the
        // widened reader acceptance makes reachable must follow the same
        // rule. Source 1.3 keeps the floor dominant in every case.
        for (v, r, cipher, expected) in [
            (1, 4, WriteCipher::PerObject(ObjectKeyAlg::Rc4), ("1.5", 0)),
            (1, 5, WriteCipher::PerObject(ObjectKeyAlg::Rc4), ("1.7", 3)),
            (1, 6, WriteCipher::PerObject(ObjectKeyAlg::Rc4), ("1.7", 8)),
            (2, 4, WriteCipher::PerObject(ObjectKeyAlg::Rc4), ("1.5", 0)),
            (4, 3, WriteCipher::PerObject(ObjectKeyAlg::Aes), ("1.4", 0)),
            (4, 4, WriteCipher::PerObject(ObjectKeyAlg::Aes), ("1.6", 0)),
            (4, 4, WriteCipher::PerObject(ObjectKeyAlg::Rc4), ("1.5", 0)),
            (4, 5, WriteCipher::PerObject(ObjectKeyAlg::Aes), ("1.7", 3)),
            (4, 6, WriteCipher::PerObject(ObjectKeyAlg::Aes), ("1.7", 8)),
            (5, 3, WriteCipher::FileKeyAes256, ("1.4", 0)),
            (5, 4, WriteCipher::FileKeyAes256, ("1.6", 0)),
            (5, 5, WriteCipher::FileKeyAes256, ("1.7", 3)),
            (5, 6, WriteCipher::FileKeyAes256, ("1.7", 8)),
        ] {
            let encryption = EncryptionParameters {
                encrypt_dict: ObjectHandle::dictionary(Vec::new()),
                file_key: vec![1; 32],
                cipher,
                encryption_v: v,
                encryption_r: r,
                id0: b"id".to_vec(),
                static_aes_iv: true,
                encrypt_metadata: true,
                metadata_ref: None,
            };
            assert_eq!(
                effective_pdf_version_and_ext_with_encryption(
                    "1.3",
                    0,
                    &WriterOptions::default(),
                    false,
                    Some(&encryption),
                ),
                expected,
                "V={v} R={r} must use qpdf's R-keyed floor"
            );
        }
    }

    #[test]
    fn effective_version_preserves_a_lower_source_version() {
        let options = WriterOptions::default();

        assert_eq!(effective_pdf_version("1.1", &options, false), "1.1");
    }

    #[test]
    fn effective_version_pair_keeps_incumbent_raw_version_when_extension_level_wins_tie() {
        // qpdf's setMinimumPDFVersion (QPDFWriter.cc:217-247) never sets
        // set_version on a numeric tie, only set_extension_level. Verified
        // against live qpdf 11.9.0: forcing a source to exactly "1.7" and
        // applying --min-version=1.7x.2 emits "%PDF-1.7" with
        // /BaseVersion /1.7 and /ExtensionLevel 2 -- the source's raw
        // spelling survives, not the tying --min-version candidate's.
        let options = WriterOptions {
            min_version: Some("1.7x".to_owned()),
            min_extension_level: Some(2),
            ..WriterOptions::default()
        };

        assert_eq!(
            effective_pdf_version_and_ext("1.7", 0, &options, false),
            ("1.7", 2)
        );
    }

    #[test]
    fn effective_version_pair_takes_the_minimum_s_raw_version_on_an_outright_numeric_win() {
        // Contrast with the tie case above: 1.7x > 1.3 numerically, so
        // qpdf's compare > 0 branch sets both the raw string and the
        // extension level from the winning --min-version candidate.
        let options = WriterOptions {
            min_version: Some("1.7x".to_owned()),
            min_extension_level: Some(2),
            ..WriterOptions::default()
        };

        assert_eq!(
            effective_pdf_version_and_ext("1.3", 0, &options, false),
            ("1.7x", 2)
        );
    }

    #[test]
    fn effective_version_pair_keeps_forced_raw_version_and_extension() {
        let options = WriterOptions {
            force_version: Some("1.7x".to_owned()),
            force_extension_level: Some(2),
            ..WriterOptions::default()
        };

        assert_eq!(
            effective_pdf_version_and_ext("1.3", 0, &options, true),
            ("1.7x", 2)
        );
    }

    #[test]
    fn effective_version_pair_ignores_an_overflowing_minimum() {
        let options = WriterOptions {
            min_version: Some("2147483648".to_owned()),
            min_extension_level: Some(2),
            ..WriterOptions::default()
        };

        assert_eq!(
            effective_pdf_version_and_ext("1.7", 0, &options, false),
            ("1.7", 0)
        );
    }

    #[test]
    fn effective_version_pair_falls_back_for_an_overflowing_source() {
        let options = WriterOptions::default();

        assert_eq!(
            effective_pdf_version_and_ext("2147483648", 0, &options, false),
            ("2147483648", 0)
        );
    }

    #[test]
    fn encryption_shape_reads_copy_encryption_handles() {
        let options = WriterOptions {
            copy_encryption: Some(CopyEncryptionSource {
                encrypt_dict: ObjectHandle::dictionary(vec![
                    (b"/V".to_vec(), ObjectHandle::integer(4)),
                    (b"/R".to_vec(), ObjectHandle::integer(4)),
                ]),
                writer_length_bits: None,
                file_key: vec![0; 16],
                padded_user_password: Vec::new(),
                id0: vec![0; 16],
                object_key_alg: ObjectKeyAlg::Rc4,
            }),
            ..WriterOptions::default()
        };

        assert_eq!(encryption_shape(&options), Some((4, 4, true)));
    }

    #[test]
    fn canonical_writer_rejects_mutually_exclusive_encryption_modes() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let options = WriterOptions {
            encrypt: Some(EncryptParams::v4_aes128(b"user", b"owner")),
            copy_encryption: Some(CopyEncryptionSource {
                encrypt_dict: ObjectHandle::dictionary(vec![
                    (b"/V".to_vec(), ObjectHandle::integer(4)),
                    (b"/R".to_vec(), ObjectHandle::integer(4)),
                ]),
                writer_length_bits: None,
                file_key: vec![0; 16],
                padded_user_password: Vec::new(),
                id0: vec![0; 16],
                object_key_alg: ObjectKeyAlg::Rc4,
            }),
            ..WriterOptions::default()
        };
        let setup = WriterSetupState {
            generated_id: None,
            encryption_parameters: None,
            source_object_stream_data: BTreeMap::new(),
            generated_compressible: None,
            generated_object_stream_sources: Vec::new(),
        };
        let error = output::with_buffer_sink(&mut Vec::new(), |out| {
            emit_canonical_pdf_inner(&mut pdf, out, &options, None, setup)
        })
        .expect_err("encrypt and copy_encryption must not be combined");
        assert!(error.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn job_writer_password_normalization_covers_each_encryption_revision() {
        let mut empty = WriterConfiguration::default();
        assert_eq!(
            empty
                .normalize_encryption_passwords(PasswordMode::Bytes)
                .unwrap(),
            Vec::<PasswordWriteNotice>::new()
        );
        for params in [
            EncryptParams::rc4(EncryptMethod::V1Rc440, b"u", b"o"),
            EncryptParams::rc4(EncryptMethod::V2Rc4128, b"u", b"o"),
            EncryptParams::rc4(EncryptMethod::V4Rc4128, b"u", b"o"),
            EncryptParams::v4_aes128(b"u", b"o"),
            EncryptParams::v5_r5(b"u", b"o"),
            EncryptParams::v5_r6(b"u", b"o"),
        ] {
            let mut configuration = WriterConfiguration::default();
            configuration.set_encryption_parameters(params);
            assert_eq!(
                configuration
                    .normalize_encryption_passwords(PasswordMode::Bytes)
                    .unwrap(),
                vec![PasswordWriteNotice::None, PasswordWriteNotice::None]
            );
        }
        let mut invalid = WriterConfiguration::default();
        invalid.set_encryption_parameters(EncryptParams::v5_r6(b"75", b"not-hex"));
        assert_eq!(
            invalid
                .normalize_encryption_passwords(PasswordMode::HexBytes)
                .unwrap(),
            vec![PasswordWriteNotice::None, PasswordWriteNotice::None]
        );
        let params = invalid
            .settings
            .encryption_parameters
            .as_ref()
            .expect("hex password parameters remain configured");
        assert_eq!(params.user_password, vec![0x75]);
        assert_eq!(params.owner_password, vec![0xe0]);
    }

    #[test]
    fn job_writer_password_normalization_reports_owner_password_errors() {
        let mut configuration = WriterConfiguration::default();
        configuration.set_encryption_parameters(EncryptParams::v4_aes128(
            "café".as_bytes().to_vec(),
            b"bad\xff".to_vec(),
        ));

        let error = configuration
            .normalize_encryption_passwords(PasswordMode::Unicode)
            .expect_err("an invalid owner password must fail Unicode normalization");
        assert!(matches!(
            error,
            crate::Error::System(message)
                if message == "supplied password is not valid UTF-8"
        ));
    }

    #[test]
    fn writer_setup_shares_encryption_parameters_across_route_slots() {
        let mut pdf = Pdf::empty().expect("empty PDF for writer setup");
        let options = WriterOptions {
            encrypt: Some(EncryptParams::v4_aes128(b"user", b"owner")),
            ..WriterOptions::default()
        };
        let setup = build_writer_setup(&mut pdf, &options).expect("setup succeeds");
        let parameters = setup
            .encryption_parameters
            .as_ref()
            .expect("explicit encryption builds one shared parameter state");
        let standard = parameters.clone().into_context(ObjectRef::new(7, 0));
        let linearized = parameters.clone().into_context(ObjectRef::new(12, 0));

        assert_eq!(standard.encrypt_ref, ObjectRef::new(7, 0));
        assert_eq!(linearized.encrypt_ref, ObjectRef::new(12, 0));
        assert_eq!(standard.id0, linearized.id0);
        assert_eq!(standard.file_key, linearized.file_key);
        assert_eq!(standard.encryption_v, linearized.encryption_v);
        assert_eq!(standard.encryption_r, linearized.encryption_r);
        assert_eq!(
            standard
                .encrypt_dict
                .try_get_key(b"/V")
                .unwrap()
                .try_as_integer()
                .unwrap(),
            linearized
                .encrypt_dict
                .try_get_key(b"/V")
                .unwrap()
                .try_as_integer()
                .unwrap()
        );
    }

    #[test]
    fn copied_v4_encryption_preserves_the_cleartext_metadata_flag() {
        let source = CopyEncryptionSource {
            encrypt_dict: ObjectHandle::dictionary(vec![
                (b"/V".to_vec(), ObjectHandle::integer(4)),
                (b"/R".to_vec(), ObjectHandle::integer(4)),
                (b"/Length".to_vec(), ObjectHandle::integer(128)),
                (b"/P".to_vec(), ObjectHandle::integer(-4)),
                (b"/O".to_vec(), ObjectHandle::string(vec![1; 32])),
                (b"/U".to_vec(), ObjectHandle::string(vec![2; 32])),
                (b"/EncryptMetadata".to_vec(), ObjectHandle::boolean(false)),
            ]),
            writer_length_bits: None,
            file_key: vec![0; 16],
            padded_user_password: Vec::new(),
            id0: vec![0; 16],
            object_key_alg: ObjectKeyAlg::Rc4,
        };
        let (dictionary, _, _, _, _) =
            canonical_copy_encryption(&source).expect("valid V=4 copy-encryption source");
        assert_eq!(
            dictionary
                .try_get_key(b"/EncryptMetadata")
                .expect("metadata flag")
                .as_boolean(),
            Some(false)
        );
    }

    #[test]
    fn copied_unsigned_permission_value_wraps_like_qpdf() {
        let source = CopyEncryptionSource {
            encrypt_dict: ObjectHandle::dictionary(vec![
                (b"/V".to_vec(), ObjectHandle::integer(5)),
                (b"/R".to_vec(), ObjectHandle::integer(6)),
                (b"/Length".to_vec(), ObjectHandle::integer(256)),
                (b"/P".to_vec(), ObjectHandle::integer(4_294_967_292)),
                (b"/O".to_vec(), ObjectHandle::string(vec![1; 48])),
                (b"/U".to_vec(), ObjectHandle::string(vec![2; 48])),
                (b"/OE".to_vec(), ObjectHandle::string(vec![3; 32])),
                (b"/UE".to_vec(), ObjectHandle::string(vec![4; 32])),
                (b"/Perms".to_vec(), ObjectHandle::string(vec![5; 16])),
            ]),
            writer_length_bits: None,
            file_key: vec![0; 32],
            padded_user_password: Vec::new(),
            id0: vec![0; 16],
            object_key_alg: ObjectKeyAlg::Aes,
        };
        let (dictionary, _, _, _, _) =
            canonical_copy_encryption(&source).expect("positive /P copy source");

        assert_eq!(
            dictionary
                .try_get_key(b"/P")
                .expect("copied /P")
                .as_integer(),
            Some(-4)
        );
    }

    /// qpdf's `QIntC` conversions raise `std::range_error` when the donor
    /// `/Length` makes `min(16, /Length / 8)` negative
    /// (`QPDF_encryption.cc:181,402`), which reaches this crate as
    /// [`crate::Error::System`].
    #[test]
    fn copied_negative_length_is_out_of_range() {
        let source = CopyEncryptionSource {
            encrypt_dict: ObjectHandle::dictionary(vec![
                (b"/V".to_vec(), ObjectHandle::integer(2)),
                (b"/R".to_vec(), ObjectHandle::integer(3)),
                (b"/Length".to_vec(), ObjectHandle::integer(-8)),
                (b"/P".to_vec(), ObjectHandle::integer(-4)),
                (b"/O".to_vec(), ObjectHandle::string(vec![1; 32])),
                (b"/U".to_vec(), ObjectHandle::string(vec![2; 32])),
            ]),
            writer_length_bits: None,
            file_key: vec![0; 16],
            padded_user_password: b"user".to_vec(),
            id0: vec![0; 16],
            object_key_alg: ObjectKeyAlg::Rc4,
        };

        let error = canonical_copy_encryption(&source)
            .expect_err("a negative /Length has no valid key length");
        assert!(
            matches!(&error, crate::Error::System(message) if message.contains("-8")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn copy_integer_reports_a_non_integer_or_missing_value() {
        let dictionary = ObjectHandle::dictionary(vec![(
            b"/V".to_vec(),
            ObjectHandle::string(b"not-an-integer".to_vec()),
        )]);
        let error = copy_integer(&dictionary, "V").expect_err("wrong type is rejected");
        assert!(error.to_string().contains("must be an integer"));
        let error = copy_integer(&dictionary, "R").expect_err("missing key is rejected");
        assert!(error.to_string().contains("must be an integer"));
    }

    #[test]
    fn pdf_writer_compression_level_changes_recompressed_output() {
        let _guard = crate::pipeline::flate::lock_compression_level_for_tests();

        fn rewrite(level: i32) -> Vec<u8> {
            let mut pdf = Pdf::open(Cursor::new(
                include_bytes!("../../../tests/fixtures/compat/lone-flate-l9.pdf").to_vec(),
            ))
            .expect("fixture must open");
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_static_id(true);
            writer.set_recompress_flate(true);
            writer.set_compression_level(level);
            writer.set_output_memory().expect("memory output");
            writer.write().expect("writer succeeds");
            writer.get_buffer().expect("memory buffer")
        }

        let level_one = rewrite(1);
        let level_nine = rewrite(9);
        let _default_level = rewrite(-1);
        crate::pipeline::flate::Flate::set_compression_level(-1).expect("reset level");
        assert_ne!(
            level_one, level_nine,
            "PdfWriter compression level must affect recompressed output"
        );
    }

    #[test]
    fn pdf_writer_reprocesses_an_invalid_compression_level_without_filtering() {
        let _guard = crate::pipeline::flate::lock_compression_level_for_tests();
        struct CompressionLevelReset;
        impl Drop for CompressionLevelReset {
            fn drop(&mut self) {
                let _ = crate::pipeline::flate::Flate::set_compression_level(-1);
            }
        }
        let _reset = CompressionLevelReset;
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        ))
        .expect("fixture must open");
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_recompress_flate(true);
        writer.set_compression_level(10);
        // get_final_version is a read-only query (no /Version dependency on
        // compression) and must not validate or mutate Flate codec state.
        assert!(
            writer.get_final_version().is_ok(),
            "get_final_version must not evaluate the configured compression level"
        );
        writer.set_output_memory().expect("memory output");
        writer
            .write()
            .expect("qpdf retries an invalid level with an unfiltered stream");
        let output = writer.get_buffer().expect("memory output buffer");
        assert!(
            !output.is_empty(),
            "the recovered writer must produce output"
        );
        assert!(
            !output
                .windows(b"/Filter /FlateDecode".len())
                .any(|window| window == b"/Filter /FlateDecode"),
            "the failed compression attempt must leave the stream unfiltered"
        );
        assert!(
            pdf.repair_diagnostics().entries().iter().any(|entry| {
                entry
                    .message_string()
                    .contains("error decoding stream data for object")
                    && entry.message_string().contains("zlib stream error")
            }),
            "qpdf reports the invalid deflate initialization as a stream warning"
        );
        assert!(
            pdf.repair_diagnostics().entries().iter().any(|entry| {
                entry
                    .message_string()
                    .contains("stream will be re-processed without filtering")
            }),
            "qpdf reports the raw retry after the invalid compression level"
        );
    }

    #[test]
    fn get_final_version_does_not_leak_compression_level_into_a_later_writer() {
        let _guard = crate::pipeline::flate::lock_compression_level_for_tests();
        crate::pipeline::flate::Flate::set_compression_level(-1).expect("reset level");

        fn rewrite_at_default_level() -> Vec<u8> {
            let mut pdf = Pdf::open(Cursor::new(
                include_bytes!("../../../tests/fixtures/compat/lone-flate-l9.pdf").to_vec(),
            ))
            .expect("fixture must open");
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_static_id(true);
            writer.set_recompress_flate(true);
            writer.set_output_memory().expect("memory output");
            writer.write().expect("writer succeeds");
            writer.get_buffer().expect("memory buffer")
        }

        let baseline = rewrite_at_default_level();

        // A caller configures an explicit level on one writer and only
        // inspects its final version, never calling write() on it.
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/lone-flate-l9.pdf").to_vec(),
        ))
        .expect("fixture must open");
        let mut inspected_writer = PdfWriter::new(&mut pdf);
        inspected_writer.set_compression_level(1);
        let _ = inspected_writer
            .get_final_version()
            .expect("get_final_version succeeds without touching Flate state");

        // A second, unrelated default-level writer must be unaffected by the
        // inspection above.
        let after_inspection = rewrite_at_default_level();
        assert_eq!(
            baseline, after_inspection,
            "querying get_final_version on one writer must not change another \
             default writer's compressed output"
        );
    }

    #[test]
    fn pclm_emits_a_synthetic_stream_for_a_page_xobject() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
        ))
        .expect("fixture must open");
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page);
        page_handle.try_is_scalar().expect("page resolves");
        let replacement = page_handle.shallow_copy().expect("page is copyable");
        let image = pdf
            .new_stream_with_data(Rc::new(b"image".to_vec()))
            .expect("image stream");
        let resources = ObjectHandle::dictionary(vec![(
            b"/XObject".to_vec(),
            ObjectHandle::dictionary(vec![(b"/Im0".to_vec(), image)]),
        )]);
        replacement
            .replace_key(b"/Resources", resources)
            .expect("replace page resources");
        pdf.replace_object(page, replacement).expect("replace page");

        let output = write_qpdf_to_memory(&mut pdf, |writer| {
            writer.set_pclm(true);
            writer.set_static_id(true);
        })
        .expect("PCLm writer succeeds");
        assert!(output
            .windows(b"q /image Do Q\n".len())
            .any(|window| { window == b"q /image Do Q\n" }));
    }

    #[test]
    fn pclm_deterministic_route_emits_source_and_synthetic_streams_with_requested_framing() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
        ))
        .expect("fixture must open");
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page);
        page_handle.try_is_scalar().expect("page resolves");
        let replacement = page_handle.shallow_copy().expect("page is copyable");
        let source_stream = pdf
            .new_stream_with_data(Rc::new(b"source-payload".to_vec()))
            .expect("source stream");
        let image = pdf
            .new_stream_with_data(Rc::new(b"image".to_vec()))
            .expect("image stream");
        replacement
            .replace_key(b"/ExtraStream", source_stream)
            .expect("attach source stream");
        replacement
            .replace_key(
                b"/Resources",
                ObjectHandle::dictionary(vec![(
                    b"/XObject".to_vec(),
                    ObjectHandle::dictionary(vec![(b"/Im0".to_vec(), image)]),
                )]),
            )
            .expect("attach page image");
        pdf.replace_object(page, replacement).expect("replace page");

        let output = write_qpdf_to_memory(&mut pdf, |writer| {
            writer.set_pclm(true);
            writer.set_deterministic_id(true);
            writer.set_extra_header_text("%coverage-header");
            writer.set_newline_before_endstream(true);
        })
        .expect("PCLm deterministic route succeeds");

        assert!(output.starts_with(b"%PDF-1.4\n%PCLm 1.0\n%coverage-header\n"));
        assert!(output
            .windows(b"source-payload\nendstream".len())
            .any(|window| window == b"source-payload\nendstream"));
        assert!(output
            .windows(b"q /image Do Q\n".len())
            .any(|window| window == b"q /image Do Q\n"));
        assert!(output
            .windows(b"/ID [".len())
            .any(|window| window == b"/ID ["));
    }

    #[test]
    fn direct_page_content_stream_marks_the_page_as_its_emission_container() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
        ))
        .expect("minimal fixture must open");
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let direct_stream = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(1))]),
            Rc::new(b"q".to_vec()),
        );
        pdf.get_object_handle(page)
            .replace_key(b"/Contents", direct_stream)
            .expect("install direct page content stream");
        let mut containers = BTreeSet::new();

        collect_content_container_refs(&mut pdf, page, &mut containers)
            .expect("collect direct content container");

        assert_eq!(containers, [page].into_iter().collect());
    }

    #[test]
    fn first_page_xref_count_rejects_param_dict_above_size() {
        assert_eq!(
            crate::linearization::writer::first_page_xref_object_count(10, 3)
                .expect("valid param-dict slot"),
            7
        );
        let error = crate::linearization::writer::first_page_xref_object_count(10, 11)
            .expect_err("param-dict slot beyond /Size must be rejected");
        assert!(matches!(
            error,
            Error::Unsupported(message)
                if message.contains("param-dict object number")
                    && message.contains("exceeds /Size")
        ));
    }

    #[test]
    fn source_objstm_extends_remaps_to_preserved_or_fallback_container() {
        let extends = ObjectRef::new(4, 0);
        let fallback = ObjectRef::new(7, 0);
        let missing = ObjectRef::new(8, 0);
        let mut source_container_to_batch = HashMap::new();
        source_container_to_batch.insert(extends, 1);
        let container_refs = vec![ObjectRef::new(20, 0), ObjectRef::new(21, 0)];
        let mut qdf_emission_renumber = HashMap::new();
        qdf_emission_renumber.insert(fallback, ObjectRef::new(30, 0));
        let mut ordinary_renumber = HashMap::new();
        ordinary_renumber.insert(fallback, ObjectRef::new(31, 0));

        assert_eq!(
            remap_source_objstm_extends(
                extends,
                &source_container_to_batch,
                &container_refs,
                false,
                &qdf_emission_renumber,
                &ordinary_renumber,
            ),
            Some(ObjectRef::new(21, 0))
        );
        assert_eq!(
            remap_source_objstm_extends(
                fallback,
                &source_container_to_batch,
                &container_refs,
                true,
                &qdf_emission_renumber,
                &ordinary_renumber,
            ),
            Some(ObjectRef::new(30, 0))
        );
        assert_eq!(
            remap_source_objstm_extends(
                fallback,
                &source_container_to_batch,
                &container_refs,
                false,
                &qdf_emission_renumber,
                &ordinary_renumber,
            ),
            Some(ObjectRef::new(31, 0))
        );
        assert_eq!(
            remap_source_objstm_extends(
                missing,
                &source_container_to_batch,
                &container_refs,
                false,
                &qdf_emission_renumber,
                &ordinary_renumber,
            ),
            None
        );
    }

    #[test]
    fn special_stream_setup_populates_qpdf_page_content_and_normalization_maps() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/qdf-contents-ref-array.pdf").to_vec(),
        ))
        .expect("contents-array fixture");
        let options = WriterOptions {
            qdf: true,
            content_normalization: true,
            ..WriterOptions::default()
        };

        let streams = initialize_special_streams(&mut pdf, &options)
            .expect("special-stream setup succeeds")
            .expect("qdf setup must create a snapshot");

        let page = ObjectRef::new(3, 0);
        assert_eq!(streams.pages, vec![page]);
        assert_eq!(streams.page_seq.get(&page), Some(&1));
        assert_eq!(streams.contents_seq.get(&ObjectRef::new(6, 0)), Some(&1));
        assert_eq!(streams.contents_seq.get(&ObjectRef::new(7, 0)), Some(&1));
        assert!(streams.normalized_streams.contains(&ObjectRef::new(6, 0)));
        assert!(streams.normalized_streams.contains(&ObjectRef::new(7, 0)));
        assert!(streams
            .content_container_refs
            .contains(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn special_stream_setup_repairs_decode_only_without_enabling_container_markers() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/qdf-contents-ref-array.pdf").to_vec(),
        ))
        .expect("contents-array fixture");
        let options = WriterOptions {
            decode_level: DecodeLevel::Generalized,
            ..WriterOptions::default()
        };

        let streams = initialize_special_streams(&mut pdf, &options)
            .expect("decode-only setup succeeds")
            .expect("decode-only setup must repair and snapshot pages");

        assert_eq!(streams.pages, vec![ObjectRef::new(3, 0)]);
        assert_eq!(streams.page_seq.get(&ObjectRef::new(3, 0)), Some(&1));
        assert!(streams.normalized_streams.contains(&ObjectRef::new(6, 0)));
        assert!(streams.normalized_streams.contains(&ObjectRef::new(7, 0)));
        assert!(streams.content_container_refs.is_empty());
    }

    #[test]
    fn direct_canonical_writer_propagates_special_stream_setup_failure() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        pdf.trailer().remove_key(b"/Root");
        let options = WriterOptions {
            qdf: true,
            ..WriterOptions::default()
        };

        let result = emit_canonical_pdf(&mut pdf, Vec::new(), &options);
        assert!(matches!(result, Err(Error::Missing("/Root"))));
    }

    #[test]
    fn root_object_emission_reconciles_shared_extensions_like_qpdf() {
        // QPDFWriter.cc:1352,1380,1425-1432 copies the Catalog container,
        // but updates an existing direct Extensions dictionary through aliases.
        let extensions = ObjectHandle::dictionary(vec![
            (b"/ADBE".to_vec(), ObjectHandle::dictionary(vec![])),
            (b"/ACME".to_vec(), ObjectHandle::integer(1)),
        ]);
        let root = ObjectHandle::dictionary(vec![(b"/Extensions".to_vec(), extensions.clone())]);
        let map = |object_ref| Ok(object_ref);
        let removed = BTreeSet::new();
        let mut output = Vec::new();
        output::with_buffer_sink(&mut output, |out| {
            root.write_root_object_with_ref_map_and_removed(out, &map, &removed, "1.7", 8, true)
        })
        .expect("root output succeeds");
        assert_eq!(
            extensions
                .try_get_key(b"/ADBE")
                .unwrap()
                .try_get_key(b"/ExtensionLevel")
                .unwrap()
                .try_as_integer()
                .unwrap(),
            Some(8)
        );

        output.clear();
        output::with_buffer_sink(&mut output, |out| {
            root.write_root_object_with_ref_map_and_removed(out, &map, &removed, "1.7", 0, true)
        })
        .expect("root output succeeds");
        assert!(extensions.try_get_key(b"/ADBE").unwrap().is_null());
        assert_eq!(
            extensions
                .try_get_key(b"/ACME")
                .unwrap()
                .try_as_integer()
                .unwrap(),
            Some(1)
        );
    }

    #[test]
    fn root_object_emission_failure_retains_shared_extensions_reconciliation() {
        let extensions =
            ObjectHandle::dictionary(vec![(b"/ACME".to_vec(), ObjectHandle::integer(1))]);
        let root = ObjectHandle::dictionary(vec![
            (b"/Extensions".to_vec(), extensions.clone()),
            (
                b"/Pages".to_vec(),
                ObjectHandle::new_indirect_unresolved(ObjectRef::new(2, 0), -1),
            ),
        ]);
        let map = |_| Err(Error::Internal("test reference mapping failure".into()));
        let result = output::with_buffer_sink(&mut Vec::new(), |out| {
            root.write_root_object_with_ref_map_and_removed(
                out,
                &map,
                &BTreeSet::new(),
                "1.7",
                8,
                true,
            )
        });
        assert!(result.is_err());
        assert_eq!(
            extensions
                .try_get_key(b"/ADBE")
                .unwrap()
                .try_get_key(b"/ExtensionLevel")
                .unwrap()
                .try_as_integer()
                .unwrap(),
            Some(8)
        );
    }

    #[test]
    fn root_object_emission_injects_adbe_without_mutating_live_catalog() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .expect("no-extension fixture");
        let root = pdf.root_handle().expect("Catalog handle");
        assert!(root
            .try_get_key(b"/Extensions")
            .expect("source Extensions")
            .is_null());

        let mut output = Vec::new();
        let map = |object_ref| Ok(object_ref);
        let removed = BTreeSet::new();
        output::with_buffer_sink(&mut output, |out| {
            root.write_root_object_with_ref_map_and_removed(out, &map, &removed, "1.7", 8, true)
        })
        .expect("root output succeeds");

        assert!(output
            .windows(b"/ADBE".len())
            .any(|window| window == b"/ADBE"));
        assert!(output
            .windows(b"/BaseVersion /1.7".len())
            .any(|window| window == b"/BaseVersion /1.7"));
        assert!(output
            .windows(b"/ExtensionLevel 8".len())
            .any(|window| window == b"/ExtensionLevel 8"));
        assert!(
            root.try_get_key(b"/Extensions")
                .expect("live Extensions")
                .is_null(),
            "output-only ADBE reconciliation must not mutate the live Catalog"
        );
    }

    #[test]
    fn plain_adbe_output_failure_keeps_the_live_catalog_unmodified() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page-no-ext.pdf").to_vec(),
        ))
        .expect("no-extension fixture");
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_minimum_pdf_version("1.7", 8);
        writer
            .set_output_writer(AlwaysFailingOutput)
            .expect("install failing output");

        assert!(writer.write().is_err(), "the output sink must fail");
        let root = pdf.root_handle().expect("Catalog remains available");
        assert!(
            root.try_get_key(b"/Extensions")
                .expect("live Extensions")
                .is_null(),
            "a failed plain write must not leave output-only ADBE on the live Catalog"
        );
    }

    #[test]
    fn qdf_generate_prepares_modified_stream_only_during_emission() {
        struct CountEofTokenFilter {
            eof_calls: Rc<std::cell::Cell<usize>>,
        }

        impl crate::token_filter::TokenFilter for CountEofTokenFilter {
            fn handle_token(
                &mut self,
                token: &crate::tokenizer::Token,
                output: &mut crate::token_filter::TokenFilterOutput<'_>,
            ) -> crate::pipeline::PipelineResult<()> {
                output.write_token(token)
            }

            fn handle_eof(
                &mut self,
                _output: &mut crate::token_filter::TokenFilterOutput<'_>,
            ) -> crate::pipeline::PipelineResult<()> {
                self.eof_calls.set(self.eof_calls.get() + 1);
                Ok(())
            }
        }

        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        ))
        .expect("one-page fixture");
        let page = pdf.get_object_handle(ObjectRef::new(3, 0));
        page.try_is_scalar().expect("page resolves");
        let content = page.try_get_key(b"/Contents").expect("page contents");
        let eof_calls = Rc::new(std::cell::Cell::new(0));
        content
            .add_token_filter(Rc::new(RefCell::new(CountEofTokenFilter {
                eof_calls: Rc::clone(&eof_calls),
            })))
            .expect("register token filter");

        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_qdf_mode(true);
        writer.set_object_stream_mode(ObjectStreamMode::Generate);
        writer.set_static_id(true);
        writer.set_output_memory().expect("memory output");
        writer
            .write()
            .expect("QDF Generate must reuse modified stream parameters");
        assert!(!writer.get_buffer().expect("written buffer").is_empty());
        assert_eq!(
            eof_calls.get(),
            1,
            "non-linearized planning must not prepare the modified stream payload"
        );
    }

    #[test]
    fn qdf_writer_does_not_invoke_a_retry_provider_during_graph_planning() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/minimal.pdf").to_vec(),
        ))
        .expect("minimal fixture");
        let calls = Rc::new(RefCell::new(Vec::new()));
        let calls_for_provider = Rc::clone(&calls);
        let stream = pdf.new_stream().expect("provider stream");
        stream
            .replace_stream_data_with_retry_callback(
                move |pipeline, suppress_warnings, will_retry| {
                    calls_for_provider
                        .borrow_mut()
                        .push((suppress_warnings, will_retry));
                    if will_retry {
                        return Ok(false);
                    }
                    pipeline.write(b"salad").map_err(Error::from)?;
                    pipeline.finish().map_err(Error::from)?;
                    Ok(true)
                },
                Some(ObjectHandle::null()),
                Some(ObjectHandle::null()),
            )
            .expect("register retry provider");
        pdf.trailer()
            .replace_key(b"/Streams", ObjectHandle::array(vec![stream]))
            .expect("attach provider stream to live trailer");

        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_qdf_mode(true);
        writer.set_static_id(true);
        writer.set_output_memory().expect("memory output");
        writer.write().expect("QDF writer succeeds");

        assert!(!writer.get_buffer().expect("written buffer").is_empty());
        assert_eq!(
            *calls.borrow(),
            vec![(false, true), (false, false)],
            "provider calls must be limited to qpdf's writer retry loop"
        );
    }
}
