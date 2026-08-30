//! qpdf correspondence: QPDFWriter.cc writer lifecycle and responsibilities shared with writer submodules and linearization.
#[path = "writer/encrypted_strings.rs"]
pub(crate) mod encrypted_strings;
#[path = "writer/encryption_state.rs"]
pub(crate) mod encryption_state;
#[path = "writer/object.rs"]
pub(crate) mod object;
#[path = "writer/object_streams/mod.rs"]
pub(crate) mod object_streams;
#[path = "writer/pclm.rs"]
pub(crate) mod pclm;
#[path = "writer/plain/mod.rs"]
pub(crate) mod plain;
#[path = "writer/reachability.rs"]
pub(crate) mod reachability;
#[path = "writer/rewrite_renumber.rs"]
pub(crate) mod rewrite_renumber;
#[path = "writer/serialize.rs"]
pub(crate) mod serialize;
mod settings;
pub(crate) use object::ObjectWriterEmission;
pub use object_streams::ObjectStreamMode;
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

use crate::encryption::{CopyEncryptionSource, EncryptParams};
use crate::linearization::writer::write_linearized_for_pdf_writer;
use crate::pdf_version::{parse_pdf_version, PdfVersion, PDF_1_2, PDF_1_5};
use crate::pipeline::{Pipeline, PlString};
use crate::{filters, Error, ObjectHandle, ObjectRef, Pdf, Result, XrefEntry, XrefForm};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

enum WriterOutput {
    Memory(Option<Vec<u8>>),
    Writer(Box<dyn Write>),
    Pipeline(Box<dyn Pipeline>),
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
            WriterOutput::Memory(_) | WriterOutput::Writer(_) => Ok(()),
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

impl Write for WriterOutputSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self.output {
            WriterOutput::Memory(buffer) => {
                buffer.get_or_insert_with(Vec::new).extend_from_slice(bytes);
                Ok(bytes.len())
            }
            WriterOutput::Writer(writer) => writer.write(bytes),
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
            WriterOutput::Writer(writer) => writer.flush(),
        }
    }
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
}

fn update_minimum_pdf_version(
    current: &mut Option<(String, i64)>,
    version: String,
    extension_level: i64,
) {
    let Some(candidate) = crate::pdf_version::parse_pdf_version(&version) else {
        // qpdf's parseVersion has no error channel and treats an invalid
        // setter value as an unusable 0.0 candidate. Ignore it here so a
        // public setter never stores a value that a later setter must unwrap.
        return;
    };
    match current {
        None => *current = Some((version, extension_level)),
        Some((current_version, current_extension_level)) => {
            let Some(current_parsed) = crate::pdf_version::parse_pdf_version(current_version)
            else {
                *current_version = version;
                *current_extension_level = extension_level;
                return;
            };
            if candidate > current_parsed
                || (candidate == current_parsed && extension_level > *current_extension_level)
            {
                *current_version = version;
                *current_extension_level = extension_level;
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
        // qdf_mode || normalize_content || stream_decode_level trigger. The
        // repair can promote direct /Kids leaves or clone duplicate leaves.
        // Prepare that graph before taking the progress snapshot so every
        // emitted repaired object is represented in events_expected.
        if options.qdf || options.content_normalization || options.decode_level != DecodeLevel::None
        {
            crate::PageDocumentHelper::new(self.pdf).get_all_pages()?;
        }
        crate::writer::configure_progress_for_pdf(
            self.pdf,
            &options,
            0,
            self.settings.linearization,
        )?; // cov:ignore: a pre-emission object-enumeration failure is surfaced by the underlying writer validation
        let result = if self.settings.linearization {
            options.qdf = false;
            let pass1_path = self.settings.linearization_pass1_filename.as_deref();
            let (mut document, result) =
                write_linearized_for_pdf_writer(self.pdf, &options, pass1_path)?;
            document.back_patch()?;
            self.output
                .as_mut()
                .expect("output was checked before writing")
                .write_complete(document.bytes)?;
            result
        } else {
            let output = self
                .output
                .as_mut()
                .expect("output was checked before writing");
            let mut sink = WriterOutputSink::new(output);
            match emit_canonical_pdf(self.pdf, &mut sink, &options) {
                Ok(result) => {
                    sink.finish_output()?;
                    result
                }
                Err(error) => return Err(sink.take_failure().unwrap_or(error)),
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

        // QPDFWriter::setEncryptionParameters and
        // QPDFWriter::copyEncryptionParameters both call generateID() before
        // installing the encryption state (QPDFWriter.cc:619 and :656). A
        // deterministic ID has no data until the writer has emitted the bytes,
        // so qpdf reports generateID's logic_error for this combination before
        // forced-version handling can disable encryption.
        if options.deterministic_id
            && (options.encrypt.is_some() || options.copy_encryption.is_some())
        {
            return Err(generate_id_without_data());
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
/// | `Preserve`  | bypass (no decode/re-encode)  | Pass dict + raw data verbatim; `apply_stream_compress_policy` is not called |
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
    /// bypasses [`apply_stream_compress_policy`] entirely, so a stream carrying
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
/// Returns `Some(policy)` meaning "call `apply_stream_compress_policy` with
/// this policy", or `None` meaning "preserve mode: skip decode/re-encode and
/// emit the stream verbatim".
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

    /// Derive the trailer `/ID[1]` (the changing identifier) from an MD5 digest
    /// of the rewritten output body — the bytes from the file header through the
    /// last body object, up to (but not including) the cross-reference table —
    /// so the identifier is stable across runs for identical input and flags.
    /// The permanent identifier `/ID[0]` is preserved from the input (ISO
    /// 32000-1 §14.4), falling back to the digest when the input has no usable
    /// `/ID`. Like `qpdf --deterministic-id`, this yields a content-derived,
    /// run-stable `/ID` and preserves the permanent identifier.
    ///
    /// The canonical rewrite honors this flag. It is mutually exclusive with
    /// [`WriterOptions::static_id`] and is rejected for encrypted output (the
    /// `/ID` feeds the encryption key, so a content-derived `/ID` would be
    /// circular) — both matching qpdf.
    ///
    /// The digest is flpdf's own scheme (a single MD5 over the body); it is
    /// **not** byte-identical to the value qpdf writes, which seeds a second MD5
    /// with the body digest plus the `/Info` strings. The `/ID` is therefore
    /// self-stable and qpdf-equivalent in behaviour, but not in exact bytes.
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
    /// source version and the linearize floor.
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
/// raises above it — so this checks `force_version` specifically. An invalid
/// (unparseable) `force_version` is ignored, matching [`effective_pdf_version`].
pub(crate) fn force_version_below_1_5(options: &WriterOptions) -> bool {
    options
        .force_version
        .as_deref()
        .and_then(parse_pdf_version)
        .is_some_and(|version| version < PDF_1_5)
}

/// Compute the effective PDF version to write given the source version, the
/// caller-supplied options, and whether the output is linearized.
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
/// 4. If `linearize` is true, apply an additional `max(…, "1.2")` floor
///    (linearized PDFs require at least PDF 1.2).
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
    linearize: bool,
    object_streams: bool,
) -> &'a str {
    // --force-version wins outright, but only when the value is a valid version string.
    // Silently ignore invalid values (same treatment as invalid min_version) so that
    // callers that cannot pre-validate do not produce a corrupted PDF header.
    if let Some(ref forced) = options.force_version {
        if parse_pdf_version(forced).is_some() {
            return forced.as_str();
        }
    }

    // Parse source; bail to source string on failure.
    let Some(mut best) = parse_pdf_version(source) else {
        return source;
    };

    // Apply --min-version floor.
    if let Some(ref min_v) = options.min_version {
        if let Some(min_parsed) = parse_pdf_version(min_v) {
            if min_parsed > best {
                best = min_parsed;
            }
        }
    }

    // Apply encryption floor (mirrors qpdf QPDFWriter::setEncryptionParametersInternal
    // at QPDFWriter.cc L806-815). AES-256 (R>=6), AES-256 legacy (R=5), AES-128
    // (R=4), RC4-128 (R=3, or R=4 without AES), RC4-40 (R<3) each require a
    // minimum header version.
    let enc_floor = encryption_version_floor(options);
    if let Some(encryption_floor) = enc_floor {
        if encryption_floor > best {
            best = encryption_floor;
        }
    }

    // Apply object-stream floor (object streams require >= 1.5).
    if object_streams && PDF_1_5 > best {
        best = PDF_1_5;
    }

    // Apply linearize floor (PDF spec requires >= 1.2).
    if linearize && PDF_1_2 > best {
        best = PDF_1_2;
    }

    // If best == source parsed, return the original source slice to avoid an
    // allocation.  Otherwise find which option string owns this version.
    if parse_pdf_version(source) == Some(best) {
        return source;
    }
    if let Some(ref min_v) = options.min_version {
        if parse_pdf_version(min_v) == Some(best) {
            return min_v.as_str();
        }
    }
    // Encryption floor matched — return a static string for the emitted version.
    // cov:ignore-start: inner-if closing braces are llvm-cov region artifacts;
    // the `return` inside is exercised by
    // effective_pdf_version_folds_each_encryption_floor_arm.
    if let Some(encryption_floor) = enc_floor {
        if encryption_floor == best {
            return best.static_version_str().unwrap_or("1.7");
        }
    }
    // cov:ignore-end
    // Object-stream floor "1.5" — reached when best == (1,5) and neither source
    // nor min_version nor encryption floor matched.
    if best == PDF_1_5 {
        return "1.5";
    }
    // Linearize floor "1.2" — only reached when best == (1,2) and neither
    // source nor min_version nor encryption floor matched.
    "1.2"
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
        } else if version >= 5 && revision >= 5 {
            PdfVersion::new(1, 7, 3)
        } else if version == 4 || revision >= 4 {
            // copyEncryptionParameters forces AES for all V>=4.
            PdfVersion::new(1, 6, 0)
        } else if revision >= 3 || version == 2 {
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
/// This is the pair-aware sibling of [`effective_pdf_version`] and delegates
/// to it for the version half. The extension level is only meaningful when
/// greater than zero; callers should injection-gate on that. `linearize` and
/// `object_streams` are threaded through unchanged.
pub(crate) fn effective_pdf_version_and_ext<'a>(
    source: &'a str,
    source_ext: i64,
    options: &'a WriterOptions,
    linearize: bool,
    object_streams: bool,
) -> (&'a str, i64) {
    // Version half: delegate.
    let ver = effective_pdf_version(source, options, linearize, object_streams);

    // Extension level half: pairwise. An input's extension level survives only
    // when that input's version *equals* the effective version — i.e. that
    // input won or tied on the version race. A bumped input (whose version was
    // outbid, including a min_version that beat the source outright) drops its
    // extension level; the pairwise rule does not carry ext across a version
    // bump. When only one side ties, its ext wins alone; when both tie
    // (source_ver == min_ver == ver) the higher of the two ext values wins.
    // When neither ties (e.g. the object-stream floor 1.5 or linearize floor
    // 1.2 bumped past both) the effective ext is 0.
    let ver_parsed = parse_pdf_version(ver);
    let source_parsed = parse_pdf_version(source);
    let min_parsed = options.min_version.as_deref().and_then(parse_pdf_version);
    // `--force-version` returns the forced value verbatim from
    // `effective_pdf_version`. qpdf treats a valid `--force-version` as an
    // exact version/extension pair: neither the source nor the caller-
    // supplied minimum extension level propagates across it.
    let forced = options
        .force_version
        .as_deref()
        .and_then(parse_pdf_version)
        .is_some();
    let enc_floor = encryption_version_floor(options);
    let source_contributes = !forced && ver_parsed.is_some() && ver_parsed == source_parsed;
    let min_contributes = !forced && ver_parsed.is_some() && ver_parsed == min_parsed;
    let enc_contributes = !forced
        && ver_parsed.is_some()
        && enc_floor.map(|version| PdfVersion::new(version.major(), version.minor(), 0))
            == ver_parsed;
    let min_ext = options.min_extension_level.unwrap_or(0);
    let enc_ext = enc_floor.map(PdfVersion::extension_level).unwrap_or(0);
    // Whichever inputs tie with the effective version each contribute their ext;
    // an input that was outbid contributes nothing. Multiple ties combine via
    // `max` — qpdf-equivalent when multiple setMinimumPDFVersion calls arrive
    // at the same version, the higher extension level wins the tie.
    let mut ext = 0i64;
    if source_contributes {
        ext = ext.max(source_ext);
    }
    if min_contributes {
        ext = ext.max(min_ext);
    }
    if enc_contributes {
        ext = ext.max(enc_ext);
    }
    if forced {
        (ver, options.force_extension_level.unwrap_or(0))
    } else {
        (ver, ext)
    }
}

/// Ensure the destination Catalog carries
/// `/Extensions << /ADBE << /BaseVersion /<version> /ExtensionLevel <lvl> >> >>`.
///
/// Mirrors qpdf's `QPDFWriter::addDeveloperExtension` handling
/// (QPDFWriter.cc L1355-1450): if the Catalog has no `/Extensions`, create a
/// direct dict carrying only `/ADBE`; if it has one (direct dict or indirect
/// reference), resolve it to a Dictionary and overwrite the `/ADBE` entry
/// only, leaving non-ADBE developer prefixes intact; write the resulting
/// Extensions dict back onto the Catalog inline as a direct value.
///
/// Callers must only invoke this when the effective extension level is > 0.
///
/// # Errors
///
/// - [`crate::Error::Missing`] if the input has no `/Root` in its trailer.
/// - Propagates canonical-handle resolution errors when materialising the
///   Catalog or an indirect `/Extensions` value.
pub(crate) fn inject_adbe_extension<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: &str,
    extension_level: i64,
) -> Result<()> {
    // cov:ignore-start: defensive /Root guard. Called from
    // emit_canonical_pdf (AFTER its own root_ref check has already
    // returned Missing("/Root")) and from
    // crate::linearization::writer::write_linearized (whose own
    // resolve_catalog_adbe_status pre-check treats a missing root as
    // `has_adbe: false` rather than
    // erroring, so this is that caller's actual root check) -- unreachable
    // in every fixture in either test module.
    let (root_ref, catalog) = writer_catalog_copy(pdf)?;
    // cov:ignore-end

    // qpdf's unparse path works on an unsafe top-level dictionary copy and
    // makes the `/Extensions` value direct before changing `/ADBE`. Copy only
    // the immediate entries here so a direct stream elsewhere in the Catalog
    // remains accepted, matching qpdf's `unsafeShallowCopy` boundary.
    let raw_extensions = catalog.try_get_key(b"/Extensions")?;
    let extensions = if let Some(entries) = raw_extensions.try_as_dictionary()? {
        ObjectHandle::dictionary(entries.into_iter().collect())
    } else {
        ObjectHandle::dictionary(Vec::new())
    };

    // qpdf preserves an existing ADBE dictionary when its version pair is
    // already the final pair, including extra keys such as `/URL`. Its
    // `prepareFileForWrite` step directizes the ADBE value before the unparse
    // decision, so do the same before comparing the two required fields.
    let mut adbe = extensions.try_get_key(b"/ADBE")?;
    if adbe.is_indirect() {
        adbe.make_direct(false)?;
        extensions.replace_key(b"/ADBE", adbe.clone())?;
    }
    let preserves_existing = adbe.try_as_dictionary()?.is_some()
        && adbe
            .try_get_key(b"/BaseVersion")?
            .try_is_name_and_equals(version.as_bytes())?
        && adbe.try_get_key(b"/ExtensionLevel")?.try_as_integer()? == Some(extension_level);
    if !preserves_existing {
        let replacement = ObjectHandle::dictionary(vec![
            (
                b"/BaseVersion".to_vec(),
                ObjectHandle::name(version.as_bytes().to_vec()),
            ),
            (
                b"/ExtensionLevel".to_vec(),
                ObjectHandle::integer(extension_level),
            ),
        ]);
        extensions.replace_key(b"/ADBE", replacement)?;
    }

    catalog.replace_key(b"/Extensions", extensions)?;
    replace_writer_catalog(pdf, root_ref, catalog)?;
    Ok(())
}

/// Reconcile `/Extensions /ADBE` when the effective extension level is 0.
/// This complements [`inject_adbe_extension`] and
/// mirrors qpdf's removal branches (QPDFWriter.cc L1408 whole-`/Extensions`
/// removal and L1432 `/ADBE`-only removal). Fires for two related cases:
/// (1) a version race (min_version bump or ObjStm floor) drops the pairwise
/// ext to 0 but the source Catalog carries an `/ADBE` entry that would
/// otherwise survive; (2) the source Catalog carries a stale / malformed
/// `/ADBE` (no `/ExtensionLevel` or non-integer) even without a race — qpdf
/// removes it based on key existence, not `/ExtensionLevel` validity, so
/// flpdf must match to preserve byte parity.
///
/// Only touches `/ADBE`; any other developer-prefix keys under `/Extensions`
/// are preserved (matching qpdf's per-prefix handling). Drops `/Extensions`
/// itself when it becomes empty after ADBE removal. If other developer keys
/// remain and the existing `/ADBE` dictionary already matches the supplied
/// version and extension level, qpdf preserves that entry and this function
/// leaves the Catalog unchanged.
///
/// # Errors
///
/// - Propagates [`Pdf::resolve`] errors when materialising the Catalog or an
///   indirect `/Extensions` value.
pub(crate) fn strip_adbe_extension<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    version: &str,
    extension_level: i64,
) -> Result<()> {
    // cov:ignore-start: defensive /Root guard, mirroring
    // inject_adbe_extension's identical comment (same two callers:
    // emit_canonical_pdf and crate::linearization::writer::write_linearized).
    let (root_ref, catalog) = writer_catalog_copy(pdf)?;
    // cov:ignore-end
    let raw_extensions = catalog.try_get_key(b"/Extensions")?;
    let Some(entries) = raw_extensions.try_as_dictionary()? else {
        return Ok(());
    };
    let extensions_was_indirect = raw_extensions.is_indirect();
    let extensions = ObjectHandle::dictionary(entries.into_iter().collect());
    let keys = extensions.try_get_keys()?;
    if !keys.contains(b"/ADBE".as_slice()) {
        return Ok(());
    }
    let has_other = keys.iter().any(|key| key.as_slice() != b"/ADBE");
    if has_other {
        let mut adbe = extensions.try_get_key(b"/ADBE")?;
        let adbe_was_indirect = adbe.is_indirect();
        if adbe_was_indirect {
            adbe.make_direct(false)?;
            extensions.replace_key(b"/ADBE", adbe.clone())?;
        }
        let valid_adbe = adbe.try_as_dictionary()?.is_some()
            && adbe
                .try_get_key(b"/BaseVersion")?
                .try_is_name_and_equals(version.as_bytes())?
            && adbe.try_get_key(b"/ExtensionLevel")?.try_as_integer()? == Some(extension_level);
        if valid_adbe {
            if extensions_was_indirect || adbe_was_indirect {
                catalog.replace_key(b"/Extensions", extensions)?;
                replace_writer_catalog(pdf, root_ref, catalog)?;
            }
            return Ok(());
        }
    }

    extensions.remove_key(b"/ADBE");
    if extensions.try_get_keys()?.is_empty() {
        catalog.remove_key(b"/Extensions");
    } else {
        catalog.replace_key(b"/Extensions", extensions)?;
    }
    replace_writer_catalog(pdf, root_ref, catalog)?;
    Ok(())
}

/// Resolve and copy the live Catalog's immediate entries for writer-owned
/// output mutations.
///
/// The legacy writer used `Pdf::resolve`, which can return a stale materialized
/// cache entry after a canonical ObjectHandle mutation. qpdf's writer operates
/// on a live `QPDFObjectHandle::unsafeShallowCopy` instead, so this boundary
/// resolves the canonical root slot, makes a direct top-level dictionary copy,
/// and leaves the final replacement to the writer Catalog replacement helper.
/// The immediate
/// entries stay shared; callers replace only top-level keys, so nested direct
/// values—including streams—are not cloned or rejected. A direct Catalog has
/// no `ObjectRef`, so the returned identity is optional and replacement is
/// performed through the live root handle.
fn writer_catalog_copy<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<(Option<ObjectRef>, ObjectHandle)> {
    let root_candidate = pdf.trailer_key_handle(b"Root");
    if root_candidate.is_null() {
        return Err(crate::Error::Missing("/Root"));
    }
    let root_ref = pdf.root_ref();
    let source = pdf.root_handle()?;
    let entries = source
        .try_as_dictionary()?
        .ok_or_else(|| crate::Error::Unsupported("Catalog is not a dictionary".to_string()))?;
    let catalog = ObjectHandle::dictionary(entries.into_iter().collect());
    Ok((root_ref, catalog))
}

/// Replace the writer-owned top-level Catalog copy without inventing an
/// indirect identity for a direct trailer `/Root`.
fn replace_writer_catalog<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root_ref: Option<ObjectRef>,
    catalog: ObjectHandle,
) -> Result<()> {
    if let Some(root_ref) = root_ref {
        pdf.replace_object(root_ref, catalog).map(|_| ())?;
        return Ok(());
    }
    let root = pdf.root_handle()?;
    root.share_value_state_with(&catalog)?;
    pdf.mark_object_handle_dirty(&root)
}

/// Capture the output-only `/Extensions` value and dirty state of the live
/// Catalog before a specialized writer mutates it for emission.
///
/// qpdf's writer may replace `/Extensions /ADBE` while preparing an output
/// object, but the canonical flpdf `PdfWriter` keeps the source `Pdf` attached
/// to the caller. Preserve permanent graph preparation while restoring only
/// this output-only Catalog key after linearization.
pub(crate) struct CatalogExtensionsSnapshot {
    root_ref: ObjectRef,
    extensions: Option<ObjectHandle>,
    was_dirty: bool,
}

/// Record the Catalog dirty state after permanent writer planning has run.
///
/// The linearization route captures the original extension handle before
/// qpdf-shaped pre-plan directization, but planning may also perform permanent
/// Catalog repairs. Those repairs must remain dirty after the output-only
/// extension mutation is restored.
pub(crate) fn record_catalog_snapshot_dirty_baseline<R: Read + Seek + 'static>(
    pdf: &Pdf<R>,
    snapshot: &mut Option<CatalogExtensionsSnapshot>,
) {
    if let Some(snapshot) = snapshot {
        snapshot.was_dirty |= pdf.is_dirty(snapshot.root_ref);
    }
}

/// Snapshot the live Catalog's output-only extension state.
pub(crate) fn snapshot_catalog_extensions<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<Option<CatalogExtensionsSnapshot>> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(None); // cov:ignore: linearization planning rejects a missing /Root first
    };
    let was_dirty = pdf.is_dirty(root_ref);
    let catalog = pdf.get_object_handle(root_ref);
    pdf.resolve(&catalog)?;
    // Raw dictionary membership, not `try_has_key`'s qpdf-semantic hasKey:
    // an explicit `/Extensions null` entry is a present key whose restored
    // shape must survive, even though qpdf's own `hasKey`/`getKeys` treat a
    // null-resolving value as absent (`libqpdf/QPDF_Dictionary.cc:98-99`).
    let extensions = catalog
        .try_as_dictionary()?
        .and_then(|dict| dict.get(b"/Extensions".as_slice()).cloned());
    Ok(Some(CatalogExtensionsSnapshot {
        root_ref,
        extensions,
        was_dirty,
    }))
}

/// Restore a previously captured output-only Catalog extension state.
pub(crate) fn restore_catalog_extensions<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    snapshot: Option<CatalogExtensionsSnapshot>,
) -> Result<()> {
    let Some(snapshot) = snapshot else {
        return Ok(()); // cov:ignore: snapshot is always Some after valid linearization planning
    };
    let (_, catalog) = writer_catalog_copy(pdf)?;
    // Raw dictionary membership, matching the snapshot side (see
    // `snapshot_catalog_extensions`).
    let current_extensions = catalog
        .try_as_dictionary()?
        .and_then(|dict| dict.get(b"/Extensions".as_slice()).cloned());
    // Identity, not serialized-value equality: the writer always allocates a
    // fresh handle when it injects or replaces `/Extensions /ADBE`, even when
    // the resulting bytes happen to match the original. Comparing by
    // `unparse()` would treat that byte-identical replacement as "unchanged"
    // and skip restoring the captured handle, leaving any external reference
    // to the original handle detached from the Catalog.
    let extensions_changed = match (&snapshot.extensions, &current_extensions) {
        (None, None) => false,
        (Some(before), Some(after)) => !before.is_same_object_as(after),
        _ => true,
    };
    if extensions_changed || (!snapshot.was_dirty && pdf.is_dirty(snapshot.root_ref)) {
        match snapshot.extensions {
            // `restore_key_raw`, not `replace_key`: this restores the exact
            // pre-write raw entry, including a literal direct null, rather
            // than performing a semantic document edit (`replace_key` treats
            // a direct null as key removal, matching qpdf's own
            // `QPDF_Dictionary::replaceKey`, which is the wrong contract
            // when undoing a temporary output-only mutation).
            Some(extensions) => catalog.restore_key_raw(b"/Extensions", extensions)?,
            None => catalog.remove_key(b"/Extensions"),
        }
        pdf.replace_object(snapshot.root_ref, catalog)?;
    }
    if !snapshot.was_dirty {
        pdf.clear_dirty(snapshot.root_ref);
    }
    Ok(())
}

/// Detect whether the destination Catalog carries `/Extensions /ADBE` in any
/// form (dict-valued or via indirect reference; regardless of `/ExtensionLevel`
/// presence or value).
///
/// Mirrors qpdf's `have_extensions_adbe = keys.count("/ADBE") > 0` check
/// (QPDFWriter.cc L1387). Used as the strip trigger for `eff_ext == 0`: when
/// the effective extension level is zero, qpdf removes stale `/ADBE` whether
/// or not the source dict carried a valid `/ExtensionLevel`; the previous
/// `adobe_extension_level() > 0` gate only fired for positive integer
/// `/ExtensionLevel` and silently passed through malformed / partial /ADBE
/// entries.
///
/// # Errors
///
/// - Propagates canonical-handle resolution errors when materialising the
///   Catalog or an indirect `/Extensions` value.
fn catalog_has_extensions_adbe<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let catalog = if let Some(root_ref) = pdf.root_ref() {
        let catalog = pdf.get_object_handle(root_ref);
        pdf.resolve(&catalog)?;
        catalog
    } else {
        let root_candidate = pdf.trailer_key_handle(b"Root");
        if root_candidate.is_null() {
            return Ok(false);
        }
        pdf.root_handle()?
    };
    if !catalog.try_has_key(b"/Extensions")? {
        return Ok(false);
    }
    let extensions = catalog.try_get_key(b"/Extensions")?;
    if extensions.try_as_dictionary()?.is_none() {
        return Ok(false);
    }
    Ok(extensions.try_get_keys()?.contains(b"/ADBE".as_slice()))
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

// ── Encryption context (flpdf-9hc.4.9) ───────────────────────────────────────

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

/// Per-write encryption state used when [`WriterOptions::encrypt`] or
/// [`WriterOptions::copy_encryption`] is set. Built once via
/// [`build_encryption_context`] or [`build_copy_encryption_context`] — at the
/// top of [`emit_canonical_pdf`] for the full-rewrite path, or inside
/// [`crate::linearization::writer::write_linearized_for_pdf_writer`] for linearized output
/// (`--encrypt` only; donor-copy/automatic source preservation are not yet
/// supported there) —
/// and consumed by the per-object emission loop + the trailer-build step.
pub(crate) struct EncryptionContext {
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
    /// Indirect reference of the freshly-allocated `/Encrypt` object. The
    /// emission loop skips this ref so the `/Encrypt` dict itself stays
    /// plaintext (PDF 1.7 §7.6.1).
    pub(crate) encrypt_ref: ObjectRef,
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

/// Resolve the document `/Catalog`'s `/Metadata` indirect reference, if any.
/// Used to exempt the XMP metadata stream from encryption under
/// `--cleartext-metadata`.
///
/// `pub(crate)`: also used by [`crate::linearization::writer::write_linearized_for_pdf_writer`],
/// which needs the same `--cleartext-metadata` exemption for linearized output.
pub(crate) fn resolve_metadata_stream_ref<R: Read + Seek>(pdf: &mut Pdf<R>) -> Option<ObjectRef> {
    let root_handle = pdf.root_handle().ok()?;
    let metadata = root_handle.try_get_key(b"/Metadata").ok()?;
    metadata.object_ref()
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
pub(crate) fn build_encryption_context(
    options: &WriterOptions,
    params: &crate::encryption::EncryptParams,
    existing_max: u32,
    metadata_ref: Option<ObjectRef>,
    id0: &[u8],
) -> Result<EncryptionContext> {
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
                p: params.permissions.to_p_bits(),
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

    // `existing_max` here is the highest already-allocated number (original
    // objects plus any ObjStm container slots reserved by the caller).
    // Adding 1 gives a safe slot that cannot collide with any emitted object.
    let encrypt_num = existing_max.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported(
            "full-rewrite encrypt: /Encrypt object number overflows u32".to_string(),
        )
    })?;

    Ok(EncryptionContext {
        encrypt_dict,
        file_key,
        cipher,
        encryption_v,
        encryption_r,
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
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
pub(crate) fn build_copy_encryption_context(
    src: &crate::encryption::CopyEncryptionSource,
    options: &WriterOptions,
    existing_max: u32,
    metadata_ref: Option<ObjectRef>,
) -> Result<EncryptionContext> {
    let (encrypt_dict, encryption_v, encryption_r, cipher) = canonical_copy_encryption(src)?;

    let encrypt_num = existing_max.checked_add(1).ok_or_else(|| {
        crate::Error::Unsupported(
            "full-rewrite copy-encryption: /Encrypt object number overflows u32".to_string(),
        )
    })?;

    let encrypt_metadata = copy_encryption_encrypts_metadata_from_dict(&encrypt_dict);

    Ok(EncryptionContext {
        encrypt_dict,
        file_key: src.file_key.clone(),
        cipher,
        encryption_v,
        encryption_r,
        encrypt_ref: ObjectRef::new(encrypt_num, 0),
        id0: src.id0.clone(),
        static_aes_iv: options.static_aes_iv,
        encrypt_metadata,
        metadata_ref: if encrypt_metadata { None } else { metadata_ref },
    })
}

/// Rebuild the dictionary qpdf emits from `copyEncryptionParameters` and
/// select the corresponding object-key cipher.
fn canonical_copy_encryption(
    src: &crate::encryption::CopyEncryptionSource,
) -> Result<(ObjectHandle, i32, i32, WriteCipher)> {
    use crate::encryption::standard::ObjectKeyAlg;

    let version = copy_integer(&src.encrypt_dict, "V")?;
    let revision = copy_integer(&src.encrypt_dict, "R")?;
    let version_i32 = i32::try_from(version).map_err(|_| {
        crate::Error::Unsupported(format!("copy-encryption /V is out of range: {version}"))
    })?;
    let revision_i32 = i32::try_from(revision).map_err(|_| {
        crate::Error::Unsupported(format!("copy-encryption /R is out of range: {revision}"))
    })?;
    let length_bits = if version == 1 {
        40
    } else {
        copy_integer(&src.encrypt_dict, "Length")?
    };
    if !(40..=256).contains(&length_bits) || length_bits % 8 != 0 {
        return Err(crate::Error::Unsupported(format!(
            "copy-encryption /Length is invalid: {length_bits} bits"
        )));
    }

    let expected_key_len = if version >= 5 {
        if version != 5 || !matches!(revision, 5 | 6) || length_bits != 256 {
            return Err(crate::Error::Unsupported(format!(
                "unsupported copy-encryption Standard handler V={version} R={revision} Length={length_bits}"
            )));
        }
        32
    } else {
        if !matches!(version, 1 | 2 | 4)
            || (version == 1 && revision != 2)
            || (version == 2 && !matches!(revision, 2 | 3))
            || (version == 4 && revision != 4)
        {
            return Err(crate::Error::Unsupported(format!(
                "unsupported copy-encryption Standard handler V={version} R={revision}"
            )));
        }
        // cov:ignore-start: length_bits is range-checked and divisible by eight;
        // the supported targets can represent every resulting key length.
        usize::try_from(length_bits / 8).map_err(|_| {
            crate::Error::Unsupported("copy-encryption key length overflows usize".into())
        })?
        // cov:ignore-end
    };
    if src.file_key.len() != expected_key_len {
        return Err(crate::Error::Unsupported(format!(
            "copy-encryption V={version} R={revision} file key must be {expected_key_len} bytes; got {}",
            src.file_key.len()
        )));
    }

    let p = copy_integer(&src.encrypt_dict, "P")?;
    let o = copy_string(&src.encrypt_dict, "O")?;
    let u = copy_string(&src.encrypt_dict, "U")?;
    let encrypt_metadata = copy_encryption_encrypts_metadata_from_dict(&src.encrypt_dict);

    let mut entries = vec![
        (b"Filter".to_vec(), ObjectHandle::name(b"Standard".to_vec())),
        (b"V".to_vec(), ObjectHandle::integer(version)),
        (b"Length".to_vec(), ObjectHandle::integer(length_bits)),
        (b"R".to_vec(), ObjectHandle::integer(revision)),
        (b"P".to_vec(), ObjectHandle::integer(p)),
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
    Ok((dict, version_i32, revision_i32, cipher))
}

fn copy_integer(dict: &ObjectHandle, key: &str) -> Result<i64> {
    let key = format!("/{key}");
    dict.try_get_key(key.as_bytes())?
        .try_as_integer()?
        .ok_or_else(|| {
            crate::Error::Unsupported(format!("copy-encryption /{key} must be an integer"))
        })
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

/// Byte length of the serialized deterministic `/ID` array `[<id0_hex><id1_hex>]`
/// for an id0 of `id0_len` bytes: `[` + (`<` + 2*id0_len hex + `>`) + (`<` + 32 hex + `>`) + `]`.
pub(crate) const fn deterministic_id_array_len(id0_len: usize) -> usize {
    1 + (1 + 2 * id0_len + 1) + (1 + 32 + 1) + 1
}
/// Serialize a deterministic `/ID` array as the fixed-width hex form qpdf emits:
/// `[<id0_hex><id1_hex>]`, with no inner spaces. The permanent identifier `id0`
/// may be any length (qpdf preserves a source `/ID[0]` verbatim regardless of
/// length); the changing identifier `id1` is always a 16-byte md5. The serialized
/// length is [`deterministic_id_array_len`]`(id0.len())`. Building the bytes by
/// hand (rather than via a generic value serializer) guarantees the hex form even when
/// a digest happens to be all-printable, so the value is always the same fixed
/// width regardless of its bytes. The classic linearized writer calls this
/// directly to emit the final identifier at each `/ID` site in its last write
/// pass (qpdf's 2-pass scheme); the ObjStm linearized writer uses it for both the
/// all-zero placeholder and the patched-in final value, whose equal width leaves
/// every later byte offset intact. (The flat write paths instead direct-write the
/// final value via [`write_deterministic_id_inline`].)
pub(crate) fn write_deterministic_id_array(out: &mut Vec<u8>, id0: &[u8], id1: &[u8; 16]) {
    out.push(b'[');
    for id in [id0, &id1[..]] {
        out.push(b'<');
        push_hex_lower(out, id);
        out.push(b'>');
    }
    out.push(b']');
}

/// Extract the source trailer's non-empty `/ID[0]` through the live qpdf-shaped
/// handle graph for writer paths whose caller has already crossed the
/// `QPDF::getTrailer` boundary.
pub(crate) fn source_permanent_id_handle(trailer: &ObjectHandle) -> Option<Vec<u8>> {
    let id = trailer.try_get_key(b"/ID").ok()?;
    source_permanent_id_value_handle(&id)
}

/// Extract qpdf's non-empty `/ID[0]` from an already-selected canonical value
/// handle. This is the one-value counterpart of `getTrailer().getKey("/ID")`.
pub(crate) fn source_permanent_id_value_handle(id: &ObjectHandle) -> Option<Vec<u8>> {
    let first = id.try_array_item(0).ok()??;
    first.try_dereference().ok()?;
    match first.as_string() {
        Some(bytes) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
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
        Err(_) => return Vec::new(), // cov:ignore: defensive resolver-error fallback
    };
    let dict = match info.try_as_dictionary() {
        Ok(Some(dict)) => dict,
        Ok(None) | Err(_) => return Vec::new(), // cov:ignore: defensive resolver-error fallback
    };
    // `ObjectHandle::try_as_dictionary` returns qpdf's lexicographically sorted
    // decoded names, matching `QPDFObjectHandle::getKeys()`.
    let mut suffix = Vec::new();
    for (_key, value) in dict {
        if value.try_dereference().is_err() {
            continue; // cov:ignore: defensive resolver-error fallback
        }
        if let Some(bytes) = value.as_string() {
            suffix.push(b' ');
            suffix.extend_from_slice(&bytes);
        }
    }
    suffix
}

/// Compute qpdf's two-level deterministic `/ID` from the serialized output.
///
/// `bytes` is the output written up to and including the `/ID` array's opening
/// `[`; `id_array_offset` is the inclusive end of the content digest range.
/// Mirrors `QPDFWriter::computeDeterministicIDData` + `generateID`:
///
/// 1. `det_data` = lowercase hex of `md5(bytes[0..=id_array_offset])`. The flat
///    writers call this from [`write_deterministic_id_inline`] with the offset
///    of the just-written `[`, so the range is inclusive of the `[` (qpdf
///    captures the running digest immediately after writing `" /ID ["`). The
///    linearized writer instead passes `bytes.len() - 1` to digest the whole
///    output, because a linearized file repeats `/ID` in several
///    trailers/xref-stream dicts and so has no single `[` cutoff; its all-zero
///    placeholder makes that whole-buffer digest depend only on the input,
///    keeping it self-stable across runs. qpdf computes this body digest with
///    `Pl_MD5`, which hashes the full byte range regardless of any embedded NUL
///    (unlike the seed in step 3).
/// 2. `seed` = `det_data` + `" QPDF "` + `info_suffix`.
/// 3. `/ID[1]` (changing identifier) = `md5(seed)`, but the seed is truncated at
///    its first NUL byte before hashing. qpdf hashes the seed with
///    `MD5::encodeString(seed.c_str())`, which treats the seed as a C string and
///    stops at the first NUL (`strlen`). The hex `det_data` and `" QPDF "` are
///    NUL-free, so any NUL originates in `info_suffix` (e.g. a UTF-16BE `/Info`
///    string, whose `00xx` code units carry NUL bytes); everything from the
///    first NUL onward is excluded from the changing identifier exactly as qpdf
///    excludes it.
/// 4. `/ID[0]` (permanent identifier) = `source_id0` (verbatim, any length) when
///    present, else a copy of `/ID[1]`.
pub(crate) fn compute_deterministic_id(
    bytes: &[u8],
    id_array_offset: usize,
    info_suffix: &[u8],
    source_id0: Option<&[u8]>,
) -> (Vec<u8>, [u8; 16]) {
    use md5::Digest as _;
    let det_data = md5::Md5::digest(&bytes[..=id_array_offset]);
    // 32 hex chars for the 16-byte digest + " QPDF " (6) + the /Info suffix.
    let mut seed = Vec::with_capacity(32 + 6 + info_suffix.len());
    push_hex_lower(&mut seed, det_data.as_slice());
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
/// far (inclusive of the `[`, the range [`compute_deterministic_id`] expects),
/// compute the two-level identifier, then write `<id0_hex><id1_hex>]`. This
/// replaces the placeholder-then-byte-search scheme on the flat write paths, so
/// a crafted placeholder-shaped byte run elsewhere can never be mistaken for the
/// real `/ID`. The emitted bytes are identical to
/// [`write_deterministic_id_array`] for the same computed id.
pub(crate) fn write_deterministic_id_inline(
    out: &mut Vec<u8>,
    info_suffix: &[u8],
    source_id0: Option<&[u8]>,
) {
    out.push(b'[');
    let id_array_offset = out.len() - 1; // index of the just-pushed `[`
    let (id0, id1) = compute_deterministic_id(out, id_array_offset, info_suffix, source_id0);
    for id in [id0.as_slice(), &id1[..]] {
        out.push(b'<');
        push_hex_lower(out, id);
        out.push(b'>');
    }
    out.push(b']');
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
    let trailer = pdf.trailer().shallow_copy()?;
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
        b"/F",
        b"/FFilter",
        b"/FDecodeParms",
        b"/W",
        b"/Index",
        b"/Length",
        b"/Filter",
        b"/DecodeParms",
        b"/XRefStm",
    ] {
        trailer.remove_key(key);
    }
    trailer.replace_key(
        b"/Size",
        ObjectHandle::integer(i64::try_from(size).map_err(|_| {
            // cov:ignore-start: supported writer object counts fit in i64
            crate::Error::Unsupported("writer trailer /Size does not fit in i64".to_string())
            // cov:ignore-end
        })?), // cov:ignore: supported writer object counts fit in i64
    )?; // cov:ignore: validated /Size replacement; LLVM attributes this continuation to the call setup
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

/// Recover the source ObjStm identity qpdf obtains from a compressed xref
/// entry, rather than from the source stream's dictionary type
/// (`QPDF.cc:2381-2390`).
fn source_objstm_container_for_batch(
    batch: &[ObjectRef],
    source_xref_entries: &BTreeMap<ObjectRef, XrefEntry>,
) -> Option<ObjectRef> {
    batch
        .iter()
        .find_map(|member| match source_xref_entries.get(member) {
            Some(XrefEntry::Compressed { stream, .. }) => Some(ObjectRef::new(*stream, 0)),
            Some(XrefEntry::Free { .. } | XrefEntry::Uncompressed { .. }) | None => None,
        })
}

/// Translate a source ObjStm's `/Extends` target into the output container
/// number. qpdf resolves this relation through the source object-stream map;
/// only when the target is not itself preserved does it fall back to the
/// ordinary output renumber map (`QPDFWriter.cc:1731-1738`).
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

/// Feed one emitted stream through qpdf's conditional encryption stage and
/// write it directly to the final output sink. The `Count` stage preserves
/// qpdf's last-byte framing decision while `PlString` is the output sink.
fn pipe_writer_stream_payload(
    out: &mut Vec<u8>,
    data: &[u8],
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<u8> {
    let mut sink = PlString::new("writer stream output", None, out);
    let mut count = crate::pipeline::count::Count::new("writer stream count", &mut sink);
    let explicit_iv = explicit_iv.or_else(|| {
        (ctx.static_aes_iv && cipher_needs_aes_iv(ctx.cipher))
            .then(crate::pipeline::aes::static_initialization_vector)
    });

    if !encrypt_stream {
        run_writer_pipeline(&mut count, data)?;
        return Ok(count.last_byte());
    }

    let mut state = encryption_state::WriterEncryptionState::new(
        true,
        ctx.file_key.clone(),
        cipher_needs_aes_iv(ctx.cipher),
        ctx.encryption_v,
        ctx.encryption_r,
    );
    state.with_object_data_key(object_ref.number, None, |state| {
        let key = state.current_data_key().ok_or_else(|| {
            // cov:ignore-start: with_object_data_key always installs a data key before this closure.
            crate::Error::Internal(
                "QPDFWriter stream encryption data key was not initialized".to_string(),
            )
            // cov:ignore-end
        })?; // cov:ignore: llvm-cov attributes this continuation to the impossible error arm.
        if key.is_empty() {
            run_writer_pipeline(&mut count, data)?;
            return Ok(());
        }

        match ctx.cipher {
            WriteCipher::PerObject(crate::encryption::standard::ObjectKeyAlg::Rc4) => {
                let mut stage =
                    crate::pipeline::rc4::PlRc4::new("rc4 stream encryption", &mut count, key)?;
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
                    )?; // cov:ignore: the no-explicit-IV AES route executes; this call continuation has no counter.
                    run_writer_pipeline(&mut stage, data)
                }
            }
        }
    })?;

    Ok(count.last_byte())
}

/// Write a stream payload through the qpdf-shaped writer pipeline, including
/// the final `endstream` framing decision based on the pipeline's last byte.
pub(crate) fn write_stream_payload_with_pipeline(
    out: &mut Vec<u8>,
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
    out: &mut Vec<u8>,
    data: &[u8],
    policy: NewlineBeforeEndstream,
    qdf_mode: bool,
    object_ref: ObjectRef,
    ctx: &EncryptionContext,
    encrypt_stream: bool,
    explicit_iv: Option<[u8; 16]>,
) -> Result<bool> {
    out.extend_from_slice(b"\nstream\n");
    let last_byte =
        pipe_writer_stream_payload(out, data, object_ref, ctx, encrypt_stream, explicit_iv)?;
    let add_newline = match policy {
        NewlineBeforeEndstream::Yes => true,
        NewlineBeforeEndstream::Never => qdf_mode && last_byte != b'\n',
    };
    if add_newline {
        out.push(b'\n');
    }
    out.extend_from_slice(b"endstream");
    Ok(add_newline)
}

pub(crate) fn emit_canonical_pdf<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    let catalog_snapshot = pdf.root_ref().and_then(|root_ref| {
        let was_dirty = pdf.is_dirty(root_ref);
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root).ok().and_then(|()| {
            let extensions = root
                .try_as_dictionary()
                .ok()?
                .and_then(|entries| entries.get(b"/Extensions".as_slice()).cloned());
            Some(CatalogExtensionsSnapshot {
                root_ref,
                extensions,
                was_dirty,
            })
        })
    });
    let direct_catalog_snapshot = if pdf.root_ref().is_none() {
        let root = pdf.trailer_key_handle(b"Root");
        if root.is_null() {
            None
        } else {
            let extensions = root
                .try_as_dictionary()?
                .and_then(|entries| entries.get(b"/Extensions".as_slice()).cloned());
            Some((root, extensions))
        }
    } else {
        None
    };
    let result = emit_canonical_pdf_inner(pdf, out, options);
    if let Some(snapshot) = catalog_snapshot {
        restore_catalog_extensions(pdf, Some(snapshot))?;
    }
    if let Some((root, original_extensions)) = direct_catalog_snapshot {
        let current_extensions = root
            .try_as_dictionary()?
            .and_then(|entries| entries.get(b"/Extensions".as_slice()).cloned());
        let changed = match (&original_extensions, &current_extensions) {
            (None, None) => false,
            (Some(before), Some(after)) => !before.is_same_object_as(after),
            _ => true,
        };
        if changed {
            match original_extensions {
                Some(extensions) => root.restore_key_raw(b"/Extensions", extensions)?,
                None => root.remove_key(b"/Extensions"),
            }
        }
    }
    result
}

fn write_pclm<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    // cov:ignore-start: emit_canonical_pdf_inner validates this combination
    // before dispatching to the private PCLm emitter.
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }
    // cov:ignore-end

    let plan = pclm::Plan::build(pdf)?;
    let version = effective_pdf_version(pdf.version(), options, false, false);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n%PCLm 1.0\n").as_bytes());
    bytes.extend_from_slice(options.extra_header_text.as_bytes());
    if !options.extra_header_text.is_empty() && !options.extra_header_text.ends_with('\n') {
        bytes.push(b'\n');
    }

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();
    let mut emitted_old_to_new = BTreeMap::<ObjectRef, ObjectRef>::new();
    let removed: BTreeSet<_> = pdf.deleted_object_refs().into_iter().collect();

    for item in &plan.items {
        match *item {
            pclm::Item::Source { source, output } => {
                let source_handle = pdf.get_object_handle(source);
                pdf.resolve(&source_handle)?;
                let offset = bytes.len();
                bytes.extend_from_slice(format!("{} 0 obj\n", output.number).as_bytes());
                let map = |object_ref| {
                    plan.old_to_new.get(&object_ref).copied().ok_or_else(|| {
                        // cov:ignore-start: Plan::build collects every live reference before
                        // this emission loop, so a valid PCLm item cannot miss this map entry.
                        crate::Error::Unsupported(format!(
                            "PCLm reference {object_ref} absent from renumber map"
                        ))
                        // cov:ignore-end
                    }) // cov:ignore: the canonical PCLm plan queues every reference before emission; LLVM maps this closure terminator to the unreachable error arm.
                };
                if source_handle.as_stream_dict().is_some() {
                    let data = source_handle.get_raw_stream_data()?;
                    source_handle.write_stream_body_with_ref_map_and_removed_and_length(
                        &mut bytes,
                        false,
                        &map,
                        &removed,
                        data.len(),
                    )?; // cov:ignore: PCLm stream emission is covered by the filtered and recovered-stream fixtures; LLVM attributes this continuation to cleanup-only code.
                    serialize::write_stream_payload(
                        &mut bytes,
                        data.as_ref(),
                        options.newline_before_endstream,
                    );
                } else {
                    source_handle
                        .write_object_with_ref_map_and_removed(&mut bytes, &map, &removed)?;
                }
                bytes.extend_from_slice(b"\nendobj\n");
                offsets.insert(output.number, (0, offset));
                emitted_old_to_new.insert(source, output);
            }
            pclm::Item::Synthetic { output } => {
                let payload = b"q /image Do Q\n".to_vec();
                let stream = ObjectHandle::stream(
                    ObjectHandle::dictionary(vec![(
                        b"Length".to_vec(),
                        ObjectHandle::integer(payload.len() as i64),
                    )]),
                    Rc::new(payload),
                );
                let offset = bytes.len();
                bytes.extend_from_slice(format!("{} 0 obj\n", output.number).as_bytes());
                write_stream_to_buf(&mut bytes, &stream, options.newline_before_endstream)?;
                bytes.extend_from_slice(b"\nendobj\n");
                offsets.insert(output.number, (0, offset));
            }
        }
        report_progress_event(options)?;
    }

    let max_object_number = offsets.keys().next_back().copied().unwrap_or(0);
    // cov:ignore-start: PCLm assigns contiguous u32 output numbers and supported
    // targets can represent the resulting object count in usize.
    let object_count = usize::try_from(max_object_number)
        .ok()
        .and_then(|number| number.checked_add(1))
        .ok_or_else(|| {
            crate::Error::Unsupported("PCLm object count does not fit in usize".to_string())
        })?;
    // cov:ignore-end
    let mut written_xref = BTreeMap::<ObjectRef, XrefEntry>::new();
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {object_count}\n").as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..object_count {
        match offsets.get(&(number as u32)) {
            Some((generation, offset)) => {
                bytes.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
                written_xref.insert(
                    ObjectRef::new(number as u32, 0),
                    XrefEntry::Uncompressed {
                        // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                        // on every supported target.
                        offset: u64::try_from(*offset).map_err(|_| {
                            crate::Error::Unsupported("PCLm xref offset does not fit u64".into())
                        })?,
                        // cov:ignore-end
                    },
                );
            }
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"), // cov:ignore: every PCLm item receives the next contiguous output number
        }
    }

    match plan.root {
        None => {
            let root = plan.direct_root.as_ref().ok_or_else(|| {
                // cov:ignore-start: Plan::build guarantees a direct Catalog
                // handle whenever its root identity is absent.
                crate::Error::Unsupported("PCLm Catalog root is inconsistent".into())
                // cov:ignore-end
            })?; // cov:ignore: Plan::build guarantees the direct Catalog handle before PCLm emission; LLVM places this continuation counter on the closure exit.
            let id_handle = pdf.trailer_key_handle(b"ID");
            let source_id0 = source_permanent_id_value_handle(&id_handle);
            let generated_id = (!options.deterministic_id)
                .then(|| generate_id_handle(source_id0.as_deref(), options.static_id));
            let trailer = build_writer_trailer_handle(
                pdf,
                object_count,
                None,
                Some(root),
                options,
                None,
                options.deterministic_id,
                generated_id.as_ref(),
            )?; // cov:ignore: validated writer trailer construction; LLVM maps this continuation to the call setup.
            let map = |object_ref: ObjectRef| {
                plan.old_to_new.get(&object_ref).copied().ok_or_else(|| {
                    // cov:ignore-start: the direct Catalog traversal that
                    // creates the plan also creates every live reference map
                    // entry reached by this serializer.
                    crate::Error::Unsupported(format!(
                        "PCLm direct /Root reference {object_ref} absent from renumber map"
                    ))
                    // cov:ignore-end
                }) // cov:ignore: the direct-root reference map is exercised; LLVM places the successful closure-exit counter on this continuation line.
            };
            if options.deterministic_id {
                let info_suffix = deterministic_id_info_suffix(pdf);
                let mut id_writer = |out: &mut Vec<u8>| {
                    write_deterministic_id_inline(out, &info_suffix, source_id0.as_deref())
                };
                trailer.write_trailer_with_ref_map(
                    &mut bytes,
                    false,
                    false,
                    Some(&mut id_writer),
                    &map,
                    &removed,
                    true,
                )?; // cov:ignore: deterministic direct-root trailer emission is exercised; LLVM maps this continuation to the call setup.
            } else {
                trailer.write_trailer_with_ref_map(
                    &mut bytes, false, false, None, &map, &removed, true,
                )?; // cov:ignore: non-deterministic direct-root trailer emission is exercised; LLVM maps this continuation to the call setup.
            }
        }
        Some(root) => {
            let id_handle = pdf.trailer_key_handle(b"ID");
            let source_id0 = source_permanent_id_value_handle(&id_handle);
            let generated_id = (!options.deterministic_id)
                .then(|| generate_id_handle(source_id0.as_deref(), options.static_id));
            let trailer = build_writer_trailer_handle(
                pdf,
                object_count,
                Some(root),
                None,
                options,
                None,
                options.deterministic_id,
                generated_id.as_ref(),
            )?; // cov:ignore: validated writer trailer construction; LLVM maps this continuation to the call setup
            let map = |object_ref| {
                // cov:ignore-start: the complete PCLm plan makes a missing trailer mapping unreachable
                plan.old_to_new.get(&object_ref).copied().ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "PCLm trailer reference {object_ref} absent from renumber map"
                    ))
                }) // cov:ignore: PCLm trailer references are covered by the canonical plan
                   // cov:ignore-end
            };
            if options.deterministic_id {
                let info_suffix = deterministic_id_info_suffix(pdf);
                let mut id_writer = |out: &mut Vec<u8>| {
                    write_deterministic_id_inline(out, &info_suffix, source_id0.as_deref())
                };
                trailer.write_trailer_with_ref_map(
                    &mut bytes,
                    false,
                    false,
                    Some(&mut id_writer),
                    &map,
                    &removed,
                    true,
                )?; // cov:ignore: validated deterministic PCLm trailer emission; LLVM maps this continuation to the call setup
            } else {
                trailer.write_trailer_with_ref_map(
                    &mut bytes, false, false, None, &map, &removed, true,
                )?; // cov:ignore: validated PCLm trailer emission; LLVM maps this continuation to the call setup
            }
        }
    }
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out.write_all(&bytes)?;
    Ok(WriterResult::new(emitted_old_to_new, written_xref))
}

fn emit_canonical_pdf_inner<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    mut out: W,
    options: &WriterOptions,
) -> Result<WriterResult> {
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }

    // A forced sub-1.5 header suppresses object-stream generation: object
    // streams are a PDF 1.5 feature and qpdf will not emit them under a forced
    // version it must not exceed (observed on qpdf 11.9.0; `--object-streams=generate
    // --force-version=1.4` is byte-identical to `--object-streams=disable
    // --force-version=1.4`). Normalize to Disable here, before the routing
    // below, so the Generate path is skipped, the planner produces no batches,
    // AND an inherited source ObjStm is dropped; the xref-form override further
    // below then rebuilds a classic table even from an xref-stream source. All
    // three modes collapse to the identical classic output, matching qpdf (whose
    // preserve/disable/generate are byte-identical under a forced sub-1.5 header).
    //
    // Generate is normalized for any encryption state (the shipped behaviour).
    // Preserve is normalized only for the non-encrypted paths: `force<1.5 +
    // encrypt` is contradictory — encryption forces its own >=1.5 floor below,
    // so it never produces sub-1.5 output — and the encrypted ObjStm handling is
    // left byte-for-byte unchanged.
    let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();
    let requested_object_streams = options.object_streams;
    let suppressed_options;
    let options = if force_version_below_1_5(options)
        && (matches!(options.object_streams, ObjectStreamMode::Generate)
            || (!encrypting && matches!(options.object_streams, ObjectStreamMode::Preserve)))
    {
        suppressed_options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..options.clone()
        };
        &suppressed_options
    } else {
        options
    };

    // Run every remaining library-level option preflight before the specialized
    // Generate or Preserve emitters below can return early.
    if options.encrypt.is_some() && options.copy_encryption.is_some() {
        return Err(crate::Error::Unsupported(
            "encrypt and copy_encryption are mutually exclusive".to_string(),
        ));
    }
    // flpdf-9hc.16.8: propagate the Adobe extension level into the destination
    // Catalog BEFORE any downstream dispatch, so every full-rewrite route sees
    // the injected Catalog.
    // When WriterOptions::min_extension_level requests an ext >= 1 (or the
    // source Catalog already carries one that survives the pairwise rule)
    // inject
    //   /Extensions << /ADBE << /BaseVersion /<ver> /ExtensionLevel <lvl> >> >>
    // so it becomes part of the Catalog the selected writer sees. A source
    // indirect /Extensions ref, if any, is inlined here and
    // drops out of the reachable graph — mirroring qpdf's writer behaviour.
    {
        let source_ver = pdf.version().to_string();
        let source_ext = pdf.adobe_extension_level().unwrap_or(0);
        // Predict whether the header floor will bump to PDF 1.5 due to
        // ObjStm emission, so the pairwise pairwise-contribution logic in
        // `effective_pdf_version_and_ext` sees the same version race that
        // the header writer will apply. Generate mode always emits ObjStm
        // through either the shared plain pipeline or this legacy excluded-mode
        // planner. Generate mode emits ObjStm under QDF as under ordinary output.
        // `Preserve` and `Disable` skip the floor here;
        // Preserve+source-has-ObjStm remains a latent edge case (walking
        // the source for eligibility would be expensive).
        let will_emit_objstm = matches!(options.object_streams, ObjectStreamMode::Generate);
        let (eff_ver, eff_ext) = effective_pdf_version_and_ext(
            &source_ver,
            source_ext,
            options,
            false,
            will_emit_objstm,
        );
        if eff_ext > 0 {
            inject_adbe_extension(pdf, eff_ver, eff_ext)?;
        } else if catalog_has_extensions_adbe(pdf)? {
            // qpdf QPDFWriter.cc L1387/L1408/L1432: when the effective extension
            // level is 0, any `/Extensions /ADBE` key must be removed —
            // whether from a prior injection that lost the pairwise version race
            // (min_version bump / ObjStm floor drops the ext to 0) or from a
            // stale/malformed source /ADBE without a valid /ExtensionLevel.
            // strip_adbe_extension handles both branches: it drops /Extensions
            // when nothing else remains, otherwise keeps it with the non-ADBE
            // developer prefixes intact.
            strip_adbe_extension(pdf, eff_ver, eff_ext)?;
        }
    }

    if options.pclm {
        return pdf.with_pclm_stream_data(|pdf| write_pclm(pdf, out, options));
    }

    if plain::eligible(pdf.is_encrypted(), options, requested_object_streams) {
        return plain::write_plain(pdf, out, options);
    }

    // Only specialized modes reach the legacy coordinator below: QDF, output or
    // copied encryption, source-encrypted input, and requested Preserve/Generate
    // suppressed to Disable by a forced version below 1.5. Its container planner
    // and generic xref emitter remain live for those explicitly excluded modes.
    let root_ref = pdf.root_ref();
    let root_handle = if root_ref.is_none() {
        let root_candidate = pdf.trailer_key_handle(b"Root");
        if root_candidate.is_null() {
            return Err(crate::Error::Missing("/Root"));
        }
        Some(pdf.root_handle()?)
    } else {
        None
    };

    // Catalog-first renumber (flpdf-9hc.32): assign output object numbers in
    // qpdf's `enqueueObjectsStandard` BFS order so that plain rewrite output is
    // byte-identical to `qpdf --static-id`. `build` borrows `pdf` mutably (lazy
    // load) and returns an owned map, releasing the borrow before the loop.
    //
    // Always use `skip_length = true`: in QDF mode the holder objects are
    // freshly assigned sequential emission numbers by the pre-scan below (not
    // reused from the source), so a prior-QDF-pass holder reachable only via a
    // /Length edge is NOT numbered here and disappears cleanly from `renumbered`.
    // In non-QDF mode this is the same behaviour as before.
    use crate::writer::rewrite_renumber::CanonicalCatalogFirstRenumber;
    // qpdf's getTrimmedTrailer applies QPDFObjectHandle::getKeys null
    // visibility before QPDFWriter::writeTrailer in every writer mode
    // (QPDFWriter.cc:1163-1192, 2009-2029). Keep that visibility separate
    // from the explicit removed-reference set below: arrays retain null
    // positions, while dictionary entries whose values resolve to null are
    // omitted regardless of QDF or encryption mode.
    let suppress_null_values = true;
    let removed_refs: BTreeSet<ObjectRef> = pdf.deleted_object_refs().into_iter().collect();
    // QPDFWriter::write calls initializeSpecialStreams() -- which repairs the
    // page tree via QPDF::getAllPages() (promoting a direct /Kids leaf to a
    // fresh indirect object, cloning a duplicate leaf) -- before any object
    // numbering (QPDFWriter.cc:2113-2115, ahead of preserveObjectStreams/
    // generateObjectStreams). Run the same repair here, before the
    // Catalog-first walk below, so any object it mints is already part of
    // the graph the walk numbers. Running this after the walk (as an earlier
    // version of this function did) left freshly-minted refs outside every
    // numbering map, causing a hard failure for a page tree that needed
    // repair in QDF/content-normalization/non-none-decode mode.
    let qdf_page_refs = if options.qdf
        || options.content_normalization
        || options.decode_level != DecodeLevel::None
    {
        Some(crate::PageDocumentHelper::new(pdf).get_all_pages()?)
    } else {
        None
    };
    let normalized_stream_refs: BTreeSet<ObjectRef> = if options.content_normalization {
        let mut refs = BTreeSet::new();
        let page_refs = qdf_page_refs
            .as_ref()
            .expect("content normalization prepares page references");
        for page_ref in page_refs {
            refs.extend(collect_content_stream_refs(pdf, *page_ref)?);
        }
        refs
    } else {
        BTreeSet::new()
    };
    let cached_stream_outputs: RefCell<BTreeMap<ObjectRef, plain::plan::CachedStreamOutput>> =
        RefCell::new(BTreeMap::new());
    // The specialized writer is a live ObjectHandle consumer. Its
    // Catalog-first walk must therefore use the same canonical graph as the
    // emission loop; the legacy raw-Object walk would parse a content holder
    // once for numbering and make the writer report its recovery warning a
    // second time when the live handle is emitted.
    // qpdf's `--qdf --preserve-unreferenced` still seeds the standard writer
    // with every input object; QDF changes formatting and ObjStm policy, not
    // the reachability setting (`QPDFWriter.cc:2907-2914`). Keep the setting
    // alive on this specialized coordinator, which is the route QDF uses.
    let stream_parameters_removed = |handle: &ObjectHandle| {
        if let Some(source) = handle.object_ref().filter(|_| handle.is_data_modified()) {
            if let Some(parameters_removed) = cached_stream_outputs
                .borrow()
                .get(&source)
                .map(|cached| cached.parameters_removed)
            {
                return Ok(parameters_removed); // cov:ignore: the canonical walk probes each source object once; emission reads this cache directly, so a repeated probe is defensive only
            }
            let (dict, data, refiltered, parameters_removed) =
                plain::body::canonical_stream_output_for_rewrite_with_status(
                    handle,
                    options,
                    normalized_stream_refs.contains(&source),
                )?; // cov:ignore: LLVM maps this covered cache-fill call terminator to a zero-count continuation region
            cached_stream_outputs.borrow_mut().insert(
                source,
                plain::plan::CachedStreamOutput {
                    dict,
                    data,
                    refiltered,
                    parameters_removed,
                    fingerprint: plain::plan::stream_cache_fingerprint(handle)?,
                },
            );
            return Ok(parameters_removed);
        }
        plain::body::canonical_stream_will_be_refiltered(handle, options)
    };
    let renumber = CanonicalCatalogFirstRenumber::build_qpdf_with_stream_policy(
        pdf,
        true,
        options.preserve_unreferenced_objects,
        &removed_refs,
        Some(&stream_parameters_removed),
    )?; // cov:ignore: llvm-cov assigns no executable counter to this multiline-call terminator; the preserve qdf call is exercised by the writer contract test.

    // The new /Root reference (always seeded first by the walk, so present).
    let new_root = root_ref.and_then(|root_ref| renumber.new_for_original(root_ref));
    if root_ref.is_some() && new_root.is_none() {
        return Err(crate::Error::Unsupported(
            "renumber: /Root absent from map".to_string(),
        ));
    }

    // Pass `false` here because full-rewrite ObjStm emission is only known
    // after planning. The required PDF 1.5 floor is applied below from the
    // final xref form, which becomes `Stream` when ObjStm batches are emitted.
    let mut version = effective_pdf_version(pdf.version(), options, false, false).to_owned();

    // ── encryption preflight (flpdf-9hc.4.9 / 4.11 / 4.16 / 4.17) ─────────
    // --encrypt supports xref-stream form and ObjStm containers (flpdf-9hc.4.16
    // / 4.17).  --copy-encryption-from still forces classic xref Table (ObjStm
    // on the copy path is not yet tested).  Reject incompatible flag
    // combinations upfront with a clear diagnostic.
    //
    // Invariant: at most ONE of encrypt / copy_encryption is set.  The CLI
    // enforces this via conflicts_with; guard here too so a library caller
    // that passes both gets a recoverable error rather than a panic.
    // `encrypting` was computed once at the top (the force<1.5 gate consults it);
    // encrypt / copy_encryption are never mutated, so it is still authoritative.

    // Capture qpdf's deterministic-`/ID` seed inputs from the live source
    // trailer before the emission loop borrows `pdf`: the permanent identifier
    // `/ID[0]` (preserved when well-formed) and the `/Info`-derived seed suffix.
    // qpdf reads these from `m->pdf.getTrailer()`, not the remapped output
    // trailer, so both are gathered here while `pdf` is free.
    let (det_id_source_id0, det_id_info_suffix): (Option<Vec<u8>>, Vec<u8>) =
        if options.deterministic_id {
            let id_handle = pdf.trailer_key_handle(b"ID");
            let id0 = source_permanent_id_value_handle(&id_handle);
            let suffix = deterministic_id_info_suffix(pdf);
            (id0, suffix)
        } else {
            (None, Vec::new())
        };

    // ── Step 1: run the ObjStm planner ───────────────────────────────────────
    // For --encrypt: ObjStm containers encrypt as a single blob per PDF 1.7
    // §7.5.7; the container stream is encrypted through the canonical writer
    // pipeline in the emission loop. Per-member string encryption is skipped
    // because members are not emitted in the main loop.
    // For --copy-encryption-from: keep ObjStm off (the copy path doesn't yet
    // allocate container numbers above the /Encrypt slot).
    let planner_options;
    let planner_config = if options.copy_encryption.is_some() {
        planner_options = WriterOptions {
            object_streams: ObjectStreamMode::Disable,
            ..options.clone()
        };
        object_streams::planner_config_from_options(&planner_options)
    } else {
        object_streams::planner_config_from_options(options)
    };
    let generated_reachable = if options.preserve_unreferenced_objects
        && planner_config.mode == ObjectStreamMode::Generate
    {
        Some(
            object_streams::compressible_objgens_qpdf_plan(pdf)?
                .eligible
                .into_iter()
                .collect::<BTreeSet<_>>(),
        )
    } else {
        None
    };
    let mut plan = object_streams::plan_object_streams_with_reachability(
        pdf,
        &planner_config,
        generated_reachable.as_ref(),
    )?; // cov:ignore: LLVM attributes this multiline planner-call terminator to the call setup; both reachability branches are exercised by writer tests

    // Drop ObjStm members that are not reachable from the trailer seed. The
    // planner draws candidates from the full live-object universe with a
    // type-only eligibility filter, so an eligible-but-unreachable object
    // (e.g. an orphan dict referenced by nothing) can be batched even though
    // the Catalog-first renumber map (which drives emission) omits it. Such an
    // object has no NEW number, so leaving it in a batch would make the
    // renumber-map lookups below fail and abort the whole write. Filtering
    // here — before the `plan.batches.is_empty()` xref-form decision below —
    // drops the orphan from every container; the main emit loop already only
    // emits objects present in the renumber map, so the orphan disappears
    // cleanly (qpdf-consistent, matching flpdf's qdf/disable paths).
    for batch in &mut plan.batches {
        batch.retain(|member| renumber.new_for_original(*member).is_some());
    }
    plan.batches.retain(|batch| !batch.is_empty());

    // QPDFWriter.cc:2141-2160 removes output-sensitive members only after
    // object-stream planning: encrypted output keeps the Catalog plain, while
    // linearized output also keeps page dictionaries plain. This legacy route
    // does not produce linearized output, so only output encryption applies.
    object_streams::filter_objstm_batches_for_output(pdf, &mut plan.batches, false, encrypting)?; // cov:ignore: legacy route validates /Root above and disables page traversal, so this helper cannot fail here

    // Xref form selection: ObjStm-resident objects need type-2 xref entries,
    // which can only live in xref streams.  When the planner emits any batch
    // we therefore force-upgrade to `Stream` even if the source used a
    // classic xref table.  An empty plan respects the source form, so a
    // Disable-mode rewrite of a Table-form input still produces a classic
    // xref table.
    let mut effective_xref_form = if plan.batches.is_empty() {
        pdf.last_xref_form()
    } else {
        XrefForm::Stream
    };

    // QDF with no object-stream batches remains a classic table: this covers
    // Disable and a Preserve input with no source ObjStm. When the selected
    // Preserve/Generate policy produces batches, the type-2 entries require
    // the xref stream just as in qpdf's ordinary writer.
    if options.qdf && plan.batches.is_empty() {
        effective_xref_form = XrefForm::Table;
    }

    // --copy-encryption-from: keep xref Table (its /Encrypt slot is at
    // existing_max+1 with no containers; xref stream support is a follow-up).
    if options.copy_encryption.is_some() {
        effective_xref_form = XrefForm::Table;
    }

    // A forced sub-1.5 header downgrades an inherited xref-stream form to a
    // classic table: cross-reference streams are a PDF 1.5 feature, and qpdf
    // keeps the forced header and rebuilds a classic xref rather than clamping
    // the version up. Gated on the non-encrypted paths — `force<1.5 + encrypt`
    // is contradictory (the /V floor below forces >=1.5), so the encrypted
    // form/version selection is left untouched. Combined with the top-of-function
    // normalization to Disable, this makes preserve/disable/generate collapse to
    // the identical classic output under force<1.5, matching qpdf 11.9.0.
    if force_version_below_1_5(options) && !encrypting {
        effective_xref_form = XrefForm::Table;
    }

    // PDF 1.5 introduced xref streams.  Bump the header floor to 1.5 whenever
    // the chosen xref form is `Stream`, overriding even an explicit
    // `--force-version` lower than 1.5.  (A non-encrypted sub-1.5 force has
    // already been downgraded to Table just above, so this clamp now fires only
    // for the encrypted paths or a >=1.5 forced/source version.)
    if matches!(effective_xref_form, XrefForm::Stream)
        && parse_pdf_version(&version).is_none_or(|current| current < PDF_1_5)
    {
        version = "1.5".to_string();
    }

    // /V-based PDF header floor.  This fires independently of xref form: even
    // when the xref-stream bump above (lines 2102-2106) has already raised the
    // header to 1.5, a V=5/R=6 output still needs this floor to push from 1.5
    // to 1.7.  For a classic-table source with no ObjStm batches the bump does
    // not fire at all, making this floor the only mechanism that prevents e.g.
    // a 1.4 input encrypted as V=4 from emitting a spec-violating 1.4 header.
    // /V 1 (R=2) ⇒ 1.3, /V 2/R=3 ⇒ 1.4, /V 4/R=4 ⇒ 1.5 or 1.6
    // depending on the crypt filter, /V 5 ⇒ 1.7.
    if let Some(params) = options.encrypt.as_ref() {
        use crate::encryption::EncryptMethod;
        let floor = match params.method {
            EncryptMethod::V1Rc440 => PdfVersion::new(1, 3, 0),
            EncryptMethod::V2Rc4128 => PdfVersion::new(1, 4, 0),
            EncryptMethod::V4Aes128 => PdfVersion::new(1, 6, 0),
            EncryptMethod::V4Rc4128 => PDF_1_5,
            EncryptMethod::V5R6Aes256 | EncryptMethod::V5R5Aes256 => PdfVersion::new(1, 7, 0),
        };
        if parse_pdf_version(&version).is_none_or(|current| current < floor) {
            version = floor.get_version().0;
        }
    }

    if options.qdf && !plan.batches.is_empty() {
        // QPDFWriter's reverse object-stream map is a std::set<QPDFObjGen>, so
        // members inside both generated and preserved containers are emitted
        // in object-number order. The standard enqueue walk determines the
        // physical order of the containers themselves. Use the same
        // ObjStm-aware walk for QDF's source-object order: the old
        // The canonical handle walk cannot see references nested in compressed
        // members, so it visits page-tree children before outline destinations
        // (`QPDFWriter.cc:1057-1118`).
        for batch in &mut plan.batches {
            batch.sort_unstable_by_key(|member| (member.number, member.generation));
        }
    }

    // ── Step 2 & 3: build member→batch lookup and allocate container numbers ─
    // Drive emission from the qpdf enqueue order: `(new_ref, old_ref)` pairs in
    // ascending reservation order. QDF must include the extra reservation made
    // for each ObjStm container and its sorted members before ordinary page
    // descendants are assigned. Non-QDF specialized routes retain the existing
    // Catalog-first map; the plain route already owns its container-aware plan.
    let renumbered: Vec<(ObjectRef, ObjectRef)> = if options.qdf && !plan.batches.is_empty() {
        use crate::writer::object_streams::ObjectStreamGroup;
        use crate::writer::rewrite_renumber::ObjectStreamRenumber;

        let groups = plan
            .batches
            .iter()
            .cloned()
            .map(|members| ObjectStreamGroup::Synthetic { members })
            .collect::<Vec<_>>();
        let object_stream_renumber = ObjectStreamRenumber::build_with_stream_policy(
            pdf,
            &groups,
            true,
            &removed_refs,
            options.preserve_unreferenced_objects,
            Some(&stream_parameters_removed),
        )?; // cov:ignore: the canonical ObjStm plan validates this shared walk before QDF emission
        let mut pairs = object_stream_renumber.pairs().collect::<Vec<_>>();
        pairs.sort_unstable_by_key(|(new_ref, _)| (new_ref.number, new_ref.generation));
        pairs
    } else {
        renumber.pairs().collect()
    };

    let existing_max: u32 = u32::try_from(renumber.len()).map_err(|_| {
        crate::Error::Unsupported("full-rewrite: renumbered object count overflows u32".to_string())
    })?;

    // A QDF container receives its number when the standard enqueue walk first
    // reaches any member of its group. This applies to both generated and
    // source-preserved groups; compact output retains its contiguous allocation
    // above the Catalog-first object range.
    let mut container_refs: Vec<ObjectRef> = if options.qdf {
        if plan.batches.is_empty() {
            Vec::new()
        } else {
            vec![ObjectRef::new(0, 0); plan.batches.len()]
        }
    } else {
        (1..=plan.batches.len())
            .map(|i| {
                let number = existing_max.checked_add(i as u32)?;
                Some(ObjectRef::new(number, 0))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                // cov:ignore-start: a supported PDF cannot allocate more than u32::MAX ObjStm batches
                crate::Error::Unsupported(
                    "full-rewrite: ObjStm container number overflows u32".to_string(),
                )
                // cov:ignore-end
            })? // cov:ignore: batch-count overflow is impossible for a supported PDF
    };

    // Member-to-batch lookup by original reference. The QDF pre-scan needs the
    // batch index before enqueue-order container numbers are known.
    let mut member_batch_index = BTreeMap::<ObjectRef, usize>::new();
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        for &member_ref in batch {
            member_batch_index.insert(member_ref, batch_idx);
        }
    }
    // Preserve the source ObjStm identity for qpdf's `/Extends` relation.
    // `PackingPlan` carries member batches for the specialized route, so
    // recover each source container from the compressed xref entry rather
    // than guessing from `/Type /ObjStm` (qpdf derives this relation from the
    // xref table, `QPDF.cc:2381-2390`). Generated batches have no source
    // container and therefore no `/Extends` value.
    let source_xref_entries = pdf.source_xref_entries();
    let source_container_for_batch: Vec<Option<ObjectRef>> =
        if options.object_streams == ObjectStreamMode::Preserve {
            plan.batches
                .iter()
                .map(|batch| source_objstm_container_for_batch(batch, &source_xref_entries))
                .collect()
        } else {
            vec![None; plan.batches.len()]
        };
    let source_container_to_batch: HashMap<ObjectRef, usize> = source_container_for_batch
        .iter()
        .enumerate()
        .filter_map(|(batch_idx, source)| source.map(|source| (source, batch_idx)))
        .collect();
    // Resolve /Metadata stream ref up front for --cleartext-metadata support.
    let metadata_ref = if options
        .encrypt
        .as_ref()
        .is_some_and(|p| !p.encrypt_metadata)
        || options
            .copy_encryption
            .as_ref()
            .is_some_and(|source| !copy_encryption_encrypts_metadata(source))
    {
        resolve_metadata_stream_ref(pdf)
    } else {
        None
    };
    // ── QDF emission pre-scan ─────────────────────────────────────────────────
    // qpdf --qdf emits each stream's /Length holder IMMEDIATELY after that
    // stream object (numbered in emission order), so file positions are strictly
    // ascending 1..N with holders interleaved. Build:
    //   • qdf_emission_renumber: old_ref → emission ObjectRef (replaces CF
    //     renumber in QDF mode for qpdf reference rewriting and trailer remapping,
    //     including members routed into an ObjStm)
    //   • qdf_holder_map: emission_stream_num → emission_holder_num
    // Prior-QDF-pass holder objects (bare integers reachable only via /Length
    // edges) were excluded from the CF renumber by skip_length=true; they do
    // not appear in `renumbered` and are not in qdf_emission_renumber, so the
    // main loop naturally skips them. Idempotence is achieved because every
    // pass produces the same emission ordering from the same graph structure.
    //
    // `skip_refs` is also declared here (before the pre-scan) because the
    // pre-scan applies the same skip conditions as the main loop.
    let skip_refs = removed_refs;
    let skip_ref_set: BTreeSet<ObjectRef> = skip_refs.iter().copied().collect();

    let mut qdf_emission_renumber: HashMap<ObjectRef, ObjectRef> = HashMap::new();
    let mut qdf_holder_map: HashMap<u32, u32> = HashMap::new();
    let mut qdf_max_emission: u32 = 0;

    if options.qdf {
        // qpdf's standard enqueue walk assigns a container as soon as it first
        // reaches one of that container's members, then reserves the complete
        // sorted member set immediately (`QPDFWriter.cc:1057-1069,1088-1115`).
        // The same rule applies to generated and source-preserved groups.
        // Ordinary objects and their QDF length holders are numbered in this
        // same walk.
        let mut next_emission = 0_u32;
        let mut assigned_batches = vec![false; plan.batches.len()];
        for (cf_ref, old_ref) in &renumbered {
            if old_ref.number == 0 || skip_refs.contains(old_ref) {
                continue; // cov:ignore: free/deleted refs don't appear in renumbered
            }
            if let Some(&batch_idx) = member_batch_index.get(old_ref) {
                if !assigned_batches[batch_idx] {
                    next_emission = next_emission.checked_add(1).ok_or_else(|| {
                        // cov:ignore-start: requires > 2^32 objects — impossible in practice
                        crate::Error::Unsupported(
                            "full-rewrite: QDF emission number overflows u32".to_string(),
                        )
                        // cov:ignore-end
                    })?; // cov:ignore: the supported PDF object space cannot overflow u32
                    container_refs[batch_idx] = ObjectRef::new(next_emission, 0);
                    assigned_batches[batch_idx] = true;

                    for member in &plan.batches[batch_idx] {
                        next_emission = next_emission.checked_add(1).ok_or_else(|| {
                            // cov:ignore-start: requires > 2^32 objects — impossible in practice
                            crate::Error::Unsupported(
                                "full-rewrite: QDF emission number overflows u32".to_string(),
                            )
                            // cov:ignore-end
                        })?; // cov:ignore: the supported PDF object space cannot overflow u32
                        qdf_emission_renumber.insert(*member, ObjectRef::new(next_emission, 0));
                    }
                }
                continue;
            }

            // Determine whether this object is a real stream (needs a holder),
            // a non-stream object, or a structural stream that the main loop
            // skips (XRef / ObjStm).
            let object_handle = pdf.get_object_handle(*old_ref);
            pdf.resolve(&object_handle)?;
            let is_real_stream = if object_handle.as_stream_dict().is_some() {
                let is_structural = object_handle.try_is_dictionary_of_type(b"XRef", b"")?
                    || object_handle.try_is_dictionary_of_type(b"ObjStm", b"")?;
                if is_structural {
                    None // cov:ignore: structural containers excluded from CF renumber by skip_length=true
                } else {
                    Some(true)
                }
            } else {
                Some(false)
            };
            let Some(is_stream) = is_real_stream else {
                continue; // cov:ignore: None only when is_structural; XRef/ObjStm excluded from renumbered by skip_length=true
            };

            next_emission = next_emission.checked_add(1).ok_or_else(|| {
                // cov:ignore-start: requires > 2^32 objects — impossible in practice
                crate::Error::Unsupported(
                    "full-rewrite: QDF emission number overflows u32".to_string(),
                )
            })?; // cov:ignore-end
            let emission_num = next_emission;
            qdf_emission_renumber.insert(*old_ref, ObjectRef::new(emission_num, cf_ref.generation));

            if is_stream {
                next_emission = next_emission.checked_add(1).ok_or_else(|| {
                    // cov:ignore-start: requires > 2^32 objects — impossible in practice
                    crate::Error::Unsupported(
                        "full-rewrite: QDF holder number overflows u32".to_string(),
                    )
                })?; // cov:ignore-end
                qdf_holder_map.insert(emission_num, next_emission);
            }
        }
        qdf_max_emission = next_emission;
    }

    // member_to_batch: ORIGINAL ObjectRef → (container_obj_num,
    // index_in_batch). Keyed on ORIGINAL refs because the main emit loop tests
    // membership against each object's ORIGINAL ref to decide whether to skip
    // it (it lives in an ObjStm instead of being emitted as a plain indirect).
    let mut member_to_batch: HashMap<ObjectRef, (u32, u32)> = HashMap::new();
    // member_new_to_batch: NEW member object number → (container_obj_num,
    // index_in_batch). Keyed on NEW numbers because type-2 xref entries are
    // written in the QDF emission-number space.
    let mut member_new_to_batch: HashMap<u32, (u32, u32)> = HashMap::new();
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_num = container_refs[batch_idx].number;
        for (idx_in_batch, &member_ref) in batch.iter().enumerate() {
            member_to_batch.insert(member_ref, (container_num, idx_in_batch as u32));
        }
    }

    // Type-2 xref entries use the same output-number space as the QDF
    // references. Non-QDF modes keep the Catalog-first numbers.
    for (&member_ref, &(container_num, index)) in &member_to_batch {
        let member_number = if options.qdf {
            qdf_emission_renumber
                .get(&member_ref)
                // cov:ignore-start: member_to_batch is built from this same complete map
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "QDF ObjStm member absent from emission map".to_string(),
                    )
                })?
                // cov:ignore-end
                .number
        } else {
            renumber
                .new_for_original(member_ref)
                // cov:ignore-start: member_to_batch is built from this complete map
                .ok_or_else(|| {
                    crate::Error::Unsupported("ObjStm member absent from renumber map".to_string())
                })?
                // cov:ignore-end
                .number
        };
        member_new_to_batch.insert(member_number, (container_num, index));
    }

    // Generate qpdf's ordinary/static identifier once before either encryption
    // key derivation or trailer emission. The complete array is reused at every
    // trailer site so the emitted /ID[0] is the exact salt used by the context.
    let generated_id = if options.deterministic_id {
        if encrypting {
            // QPDFWriter::generateID is called by the encryption setup before
            // the deterministic MD5 pipeline can produce its data.
            return Err(generate_id_without_data());
        }
        None
    } else if options.copy_encryption.is_some() {
        None
    } else {
        let id_handle = pdf.trailer_key_handle(b"ID");
        let source_id0 = source_permanent_id_value_handle(&id_handle);
        Some(generate_id_handle(source_id0.as_deref(), options.static_id))
    };

    // ── flpdf-9hc.4.9 / 4.11 / 4.16: encryption context ────────────────────
    // Built ONCE up front so /ID[0] is decided before any object is encrypted.
    // Compact /Encrypt follows existing objects and generated ObjStm containers.
    // QDF /Encrypt follows the final interleaved /Length holder from the pre-scan.
    let encrypt_ctx: Option<EncryptionContext> = if let Some(ref params) = options.encrypt {
        let base_for_encrypt = if options.qdf {
            qdf_max_emission
        } else {
            // cov:ignore-start: contiguous object and batch counts cannot approach u32::MAX in a supported process.
            let containers_len = u32::try_from(plan.batches.len()).map_err(|_| {
                crate::Error::Unsupported(
                    "full-rewrite encrypt: ObjStm batch count overflows u32".to_string(),
                )
            })?;
            existing_max.checked_add(containers_len).ok_or_else(|| {
                crate::Error::Unsupported(
                    "full-rewrite encrypt: /Encrypt object number overflows u32".to_string(),
                )
            })?
            // cov:ignore-end
        };
        let id0 = generated_id
            .as_ref()
            .and_then(ObjectHandle::as_array)
            .and_then(|values| values.first().and_then(ObjectHandle::as_string))
            .ok_or_else(|| {
                // cov:ignore-start: generate_id_handle always returns a valid two-string array here
                crate::Error::Unsupported(
                    "full-rewrite: ordinary/static ID generator returned an invalid /ID array"
                        .to_string(),
                )
                // cov:ignore-end
            })?; // cov:ignore: invalid ID guard is unreachable after generate_id_handle
        let context =
            build_encryption_context(options, params, base_for_encrypt, metadata_ref, &id0);
        Some(context?)
    } else if let Some(ref src) = options.copy_encryption {
        let base_for_encrypt = if options.qdf {
            qdf_max_emission
        } else {
            existing_max
        };
        Some(build_copy_encryption_context(
            src,
            options,
            base_for_encrypt,
            metadata_ref,
        )?)
    } else {
        None
    };
    let mut encrypted_strings = encrypt_ctx
        .as_ref()
        .map(encrypted_strings::EncryptedStringEmitter::from_context);

    // ── QDF page/contents marker pre-scan ─────────────────────────────────────
    // qpdf --qdf emits two page-context comments to help human readers:
    //   • "%% Page N\n"              — immediately before each Page dict's
    //                                   "M G obj" line (N is 1-based page order)
    //   • "%% Contents for page N\n" — immediately before each content stream's
    //                                   "M G obj" line (N is the owning page's
    //                                   1-based order); a page's /Contents may
    //                                   be a lone reference or an array of
    //                                   references, and every element shares the
    //                                   same page number.
    // The contents map also selects exactly the indirect page-content streams
    // eligible for qpdf content normalization. Maps are keyed on ORIGINAL
    // ObjectRefs (matching how the emit loop compares via `old_ref`). Page
    // markers are populated and emitted only in QDF mode. They ride ahead of
    // "%% Original object ID:" and are NOT suppressed by
    // no_original_object_ids. Mirrors qpdf 11.9.0 QPDFWriter.cc:1774-1785.
    //
    // `contents_seq` contains only indirect stream refs returned by the
    // canonical page-content resolver. `content_container_refs` identifies
    // page dictionaries and indirect array holders that contain direct Stream
    // values; those values have no ObjectRef of their own and must be
    // normalized in the containing object during emission.
    let (page_seq, contents_seq, content_container_refs): (
        HashMap<ObjectRef, u32>,
        HashMap<ObjectRef, u32>,
        BTreeSet<ObjectRef>,
    ) = if options.qdf || options.content_normalization {
        let mut page_seq: HashMap<ObjectRef, u32> = HashMap::new();
        let mut contents_seq: HashMap<ObjectRef, u32> = HashMap::new();
        let mut content_container_refs = BTreeSet::new();
        // QPDFWriter::initializeSpecialStreams delegates page enumeration to
        // QPDF::getAllPages(), whose live ObjectHandle lookup accepts a direct
        // Catalog /Pages dictionary (QPDFWriter.cc:1916; QPDF_pages.cc:47-71).
        // The repair-and-enumerate pass already ran once, before the
        // Catalog-first numbering walk above, so any object it mints is
        // numbered; reuse that snapshot instead of repairing (a no-op the
        // second time) and enumerating again.
        let page_refs = qdf_page_refs
            .as_ref()
            .expect("qdf_page_refs is Some whenever options.qdf || options.content_normalization");
        for (idx, page_ref) in page_refs.iter().enumerate() {
            let seq = (idx as u32).saturating_add(1);
            if options.qdf {
                page_seq.insert(*page_ref, seq);
            }
            // Enumerate page content streams through the canonical
            // ObjectHandle graph. Keep the live stream handle's original
            // indirect identity for the emission loop; qpdf does not chase
            // flpdf-only reference-holder chains here.
            for content_ref in collect_content_stream_refs(pdf, *page_ref)? {
                contents_seq.insert(content_ref, seq);
            }
            collect_content_container_refs(pdf, *page_ref, &mut content_container_refs)?;
        }
        (page_seq, contents_seq, content_container_refs)
    } else {
        (HashMap::new(), HashMap::new(), BTreeSet::new())
    };

    // In QDF mode, /Root's ref in the trailer is in emission-space; rebind
    // new_root from the qdf_emission_renumber map so trailer rewriting and the
    // explicit trailer.insert("Root", ...) both use the same emission number.
    let new_root = if let Some(root_ref) = root_ref {
        if options.qdf {
            Some(
                qdf_emission_renumber
                    .get(&root_ref)
                    .copied()
                    .ok_or_else(|| {
                        // cov:ignore-start: /Root is always reachable from the BFS seed, so it
                        // is always in renumbered and therefore always in qdf_emission_renumber.
                        crate::Error::Unsupported(
                            "QDF emission: /Root absent from emission map".to_string(),
                        )
                        // cov:ignore-end
                    })?, // cov:ignore: /Root is always seeded before QDF emission, so this validated map lookup has no reachable error continuation.
            )
        } else {
            new_root
        }
    } else {
        None
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    if options.pclm {
        bytes.extend_from_slice(b"%PCLm 1.0\n"); // cov:ignore: PCLm returns through write_pclm before this coordinator
    } else {
        bytes.extend_from_slice(QPDF_BINARY_MARKER);
    }
    if options.qdf {
        bytes.extend_from_slice(b"%QDF-1.0\n");
        bytes.extend_from_slice(b"\n");
    }
    bytes.extend_from_slice(options.extra_header_text.as_bytes());

    let mut offsets = BTreeMap::<u32, (u16, usize)>::new();
    let mut emitted_old_to_new = BTreeMap::<ObjectRef, ObjectRef>::new();
    let qdf_body_start = bytes.len();
    let mut qdf_main_chunks = BTreeMap::<ObjectRef, (usize, usize)>::new();
    let mut qdf_container_chunks = BTreeMap::<usize, (usize, usize)>::new();

    for (new_ref, old_ref) in &renumbered {
        // Never emit object 0 or any free/deleted entry as a body object (qpdf
        // parity, all modes). The xref free-list head and any free rows are
        // still written into the regenerated `xref` table below.
        if old_ref.number == 0 || skip_refs.contains(old_ref) {
            continue;
        }

        // ── Step 4: skip members that will be routed into an ObjStm batch ───
        if member_to_batch.contains_key(old_ref) {
            continue;
        }

        // In QDF mode, look up the emission-space ObjectRef. Objects absent
        // from qdf_emission_renumber (prior-QDF-pass holders excluded by
        // skip_length=true in CF renumber) are skipped here, ensuring
        // idempotence. In non-QDF mode emit_ref == *new_ref.
        let emit_ref = if options.qdf {
            match qdf_emission_renumber.get(old_ref) {
                Some(&r) => r,
                None => continue, // cov:ignore: pre-scan and main loop have symmetric skips; unreachable in valid PDFs
            }
        } else {
            *new_ref
        };

        // Resolve the live ObjectHandle once. qpdf's writer keeps this handle
        // as the source of truth and remaps only reference tokens while
        // unparsing; it does not materialize the whole object graph before
        // emission.
        let object_handle = pdf.get_object_handle(*old_ref);
        pdf.resolve(&object_handle)?;
        let is_stream = object_handle.as_stream_dict().is_some();

        // Direct `/Contents` streams have no terminal ObjectRef to put in
        // `contents_seq`. Their owning page/array holder uses the dedicated
        // handle-native content-container serializer below; this applies in
        // both QDF and normalization modes because a generic child serializer
        // intentionally emits only a direct stream's dictionary.
        let content_container = content_container_refs.contains(old_ref);
        // Skip xref-stream and ObjStm container objects — we'll rebuild the
        // structural streams from scratch below. Handle predicates preserve
        // qpdf's live dictionary lookup without resolving a legacy `Object`.
        if is_stream
            && (object_handle.try_is_dictionary_of_type(b"XRef", b"")?
                || object_handle.try_is_dictionary_of_type(b"ObjStm", b"")?)
        {
            continue; // cov:ignore: structural streams are rebuilt by their dedicated loops below
        }

        // Duplicate detection: `offsets` is keyed on the emitted number.
        if offsets.contains_key(&emit_ref.number) {
            // cov:ignore-start: qdf_emission_renumber assigns unique sequential numbers,
            // so collisions cannot occur in valid PDFs; this is a bug-detection guard.
            return Err(crate::Error::Unsupported(format!(
                "duplicate object number {} in xref table",
                emit_ref.number
            )));
            // cov:ignore-end
        }

        // QDF page/contents markers ride ahead of "%% Original object ID:" and
        // remain even under no_original_object_ids. Mirrors qpdf 11.9.0
        // QPDFWriter.cc:1774-1785. Keyed on original refs (old_ref).
        let qdf_chunk_start = bytes.len();
        if options.qdf {
            if let Some(&seq) = page_seq.get(old_ref) {
                bytes.extend_from_slice(format!("%% Page {seq}\n").as_bytes());
            }
            if let Some(&seq) = contents_seq.get(old_ref) {
                bytes.extend_from_slice(format!("%% Contents for page {seq}\n").as_bytes());
            }
        }

        // QDF per-object comment: "%% Original object ID: N G"
        // Emitted immediately before the "N G obj" line so human readers can
        // locate objects without consulting the xref table.  Mirrors qpdf
        // 11.9.0 --qdf output.  Suppressed when no_original_object_ids=true.
        // The xref offset below is recorded AFTER the comment so it still
        // points at the "N G obj" line, not at the comment.
        // The comment records the ORIGINAL object id (qpdf prints the pre-
        // renumber number here), so use `old_ref`.
        if options.qdf && !options.no_original_object_ids {
            bytes.extend_from_slice(
                format!(
                    "%% Original object ID: {} {}\n",
                    old_ref.number, old_ref.generation
                )
                .as_bytes(),
            );
        }

        // The body header uses the emitted number.
        let emit_offset = bytes.len();
        bytes.extend_from_slice(
            format!("{} {} obj\n", emit_ref.number, emit_ref.generation).as_bytes(),
        );

        // Will be set to Some((holder_num, len_value, ignore_newline)) for QDF
        // streams so we can emit the marker and holder immediately after the
        // stream's endobj.
        let mut qdf_holder_to_emit: Option<(u32, i64, bool)> = None;

        let map = |object_ref: ObjectRef| {
            if options.qdf {
                qdf_emission_renumber
                    .get(&object_ref)
                    .copied()
                    .ok_or_else(|| {
                        // cov:ignore-start: catalog-first planning inserts every live QDF reference
                        crate::Error::Unsupported(format!(
                            "full-rewrite: QDF reference {object_ref} absent from emission map"
                        ))
                        // cov:ignore-end
                    }) // cov:ignore: catalog-first planning makes every QDF reference resolvable
            } else {
                renumber.new_for_original(object_ref).ok_or_else(|| {
                    // cov:ignore-start: catalog-first planning inserts every live reference
                    crate::Error::Unsupported(format!(
                        "full-rewrite: reference {object_ref} absent from renumber map"
                    ))
                    // cov:ignore-end
                }) // cov:ignore: catalog-first planning makes every reference resolvable
            }
        };
        let removed_refs: BTreeSet<ObjectRef> = skip_refs.iter().copied().collect();

        if content_container {
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_content_container_with_ref_map(
                    &mut bytes,
                    emit_ref,
                    None,
                    &object_handle,
                    options,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: LLVM does not attribute the successful encrypted emitter continuation
            } else {
                plain::body::emit_content_container_from_handle_with_ref_map(
                    &object_handle,
                    options,
                    &mut bytes,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: LLVM does not attribute the successful plain emitter continuation
            }
        } else if is_stream {
            // This is the qpdf stream writer's live-handle path: filtering and
            // payload framing are decided from the stream handle, while the
            // dictionary serializer remaps only child reference tokens.
            let cached = cached_stream_outputs
                .borrow()
                .get(old_ref)
                .map(|cached| (cached.dict.clone(), cached.data.clone(), cached.refiltered));
            let (stream_dict, stream_data, refiltered) = if let Some(cached) = cached {
                cached
            } else {
                plain::body::canonical_stream_output_for_rewrite(
                    &object_handle,
                    options,
                    options.content_normalization && contents_seq.contains_key(old_ref),
                )? // cov:ignore: canonical stream output is validated before this success continuation
            };
            let stream_encryption = encrypt_ctx
                .as_ref()
                .filter(|ctx| emit_ref != ctx.encrypt_ref);
            let encrypt_stream = stream_encryption
                .is_some_and(|ctx| ctx.encrypt_metadata || ctx.metadata_ref != Some(*old_ref));
            let stream_dict = stream_dict;
            let mut stream_length = stream_data.len();
            if let Some(ctx) = stream_encryption {
                adjust_aes_stream_length(&mut stream_length, ctx, encrypt_stream)?;
            }
            stream_dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                    // cov:ignore-start: an allocatable stream payload fits in i64
                    crate::Error::Unsupported("stream /Length does not fit in i64".to_string())
                    // cov:ignore-end
                })?), // cov:ignore: an allocatable stream payload fits in i64
            )?; // cov:ignore: validated stream /Length replacement; LLVM maps this continuation to the call setup

            let holder_ref = if options.qdf {
                let holder_num =
                    qdf_holder_map
                        .get(&emit_ref.number)
                        .copied()
                        .ok_or_else(|| {
                            // cov:ignore-start: the QDF pre-scan creates a holder for every emitted stream
                            crate::Error::Unsupported(format!(
                                "full-rewrite: QDF holder not found for stream at emission {}",
                                emit_ref.number
                            ))
                            // cov:ignore-end
                        })?; // cov:ignore: the QDF pre-scan creates a holder for every emitted stream
                Some(ObjectRef::new(holder_num, 0))
            } else {
                None
            };
            let stream_options =
                encrypted_strings::StreamDictOptions::new(options.qdf, refiltered, encrypt_stream);
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_stream_dict_with_ref_map(
                    &mut bytes,
                    emit_ref,
                    None,
                    &stream_dict,
                    stream_options,
                    &map,
                    &removed_refs,
                    holder_ref,
                )?; // cov:ignore: handle-native stream dictionary route; LLVM maps the call continuation here
            } else if options.qdf {
                stream_dict.write_stream_body_qdf_with_ref_map_and_removed_and_length(
                    &mut bytes,
                    0,
                    &map,
                    &removed_refs,
                    holder_ref,
                )?; // cov:ignore: handle-native QDF stream dictionary route; LLVM maps the call continuation here
            } else {
                stream_dict.write_stream_body_with_ref_map_and_removed(
                    &mut bytes,
                    refiltered,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: handle-native stream dictionary route; LLVM maps the call continuation here
            }

            let added_newline = if let Some(ctx) = stream_encryption {
                write_stream_payload_with_pipeline_qdf(
                    &mut bytes,
                    &stream_data,
                    options.newline_before_endstream,
                    options.qdf,
                    emit_ref,
                    ctx,
                    encrypt_stream,
                    None,
                )? // cov:ignore: encrypted stream payload route; LLVM maps the call continuation here
            } else {
                serialize::write_stream_payload_with_qdf(
                    &mut bytes,
                    &stream_data,
                    options.newline_before_endstream,
                    options.qdf,
                );
                serialize::framing_adds_newline_with_qdf(
                    &stream_data,
                    options.newline_before_endstream,
                    options.qdf,
                )
            };
            if let Some(holder_ref) = holder_ref {
                qdf_holder_to_emit = Some((
                    holder_ref.number,
                    i64::try_from(stream_length).unwrap_or(i64::MAX),
                    added_newline,
                ));
            }
        } else {
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_object_with_ref_map(
                    &mut bytes,
                    emit_ref,
                    None,
                    &object_handle,
                    options.qdf,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: encrypted handle-object route; LLVM maps the call continuation here
            } else if options.qdf {
                object_handle.write_object_qdf_with_ref_map_and_removed(
                    &mut bytes,
                    0,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: QDF handle-object route; LLVM maps the call continuation here
            } else {
                object_handle.write_object_with_ref_map_and_removed(
                    &mut bytes,
                    &map,
                    &removed_refs,
                )?; // cov:ignore: compact handle-object route; LLVM maps the call continuation here
            }
        }

        bytes.extend_from_slice(b"\nendobj\n");
        // QDF framing (flpdf-9hc.6.10): qpdf `--qdf` separates every indirect
        // object with one blank line (`endobj\n\n%% Original object ID:` …, and
        // `endobj\n\nxref` before the xref table). The trailing blank line is
        // also emitted before the next holder/ObjStm object and, because
        // `xref_offset` is captured immediately after the loops, before the
        // `xref` keyword for the final object — matching qpdf byte-for-byte.
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(emit_ref.number, (emit_ref.generation, emit_offset));
        emitted_old_to_new.insert(*old_ref, ObjectRef::new(emit_ref.number, 0));
        report_progress_event(options)?;

        // QDF: emit the length-holder object IMMEDIATELY after its stream's
        // endobj + blank line, numbered in sequential emission order so that
        // object file positions are strictly ascending 1..N (qpdf 11.9.0
        // behaviour). No "%% Original object ID:" comment for holder objects
        // (they are synthetic; qpdf only emits that comment for source objects).
        if let Some((hnum, hlen, ignore_newline)) = qdf_holder_to_emit {
            if ignore_newline {
                bytes.extend_from_slice(b"%QDF: ignore_newline\n");
            }
            let h_offset = bytes.len();
            bytes.extend_from_slice(format!("{hnum} 0 obj\n{hlen}\nendobj\n").as_bytes());
            bytes.push(b'\n'); // QDF inter-object blank line
            offsets.insert(hnum, (0, h_offset));
        }
        if options.qdf {
            qdf_main_chunks.insert(*old_ref, (qdf_chunk_start, bytes.len()));
        }
    }

    // ── Step 5: emit each ObjStm container ───────────────────────────────────
    for (batch_idx, batch) in plan.batches.iter().enumerate() {
        let container_ref = container_refs[batch_idx];
        // Resolve each member as a live ObjectHandle and remap only its child
        // reference tokens during emission. The encrypted branch remains on
        // the legacy callback until the handle-aware string-writer adapter is
        // wired into the same boundary.
        let mut handles = Vec::with_capacity(batch.len());
        for &old in batch {
            let handle = pdf.get_object_handle(old);
            pdf.resolve(&handle)?;
            let new = if options.qdf {
                qdf_emission_renumber
                    .get(&old)
                    .copied()
                    // cov:ignore-start: handles are selected from this complete QDF emission map
                    .ok_or_else(|| {
                        crate::Error::Unsupported(
                            "QDF ObjStm member absent from emission map".to_string(),
                        )
                    })?
                // cov:ignore-end
            } else {
                renumber
                    .new_for_original(old)
                    // cov:ignore-start: handles are selected from this complete renumber map
                    .ok_or_else(|| {
                        crate::Error::Unsupported(
                            "ObjStm member absent from renumber map".to_string(),
                        )
                    })?
                // cov:ignore-end
            };
            emitted_old_to_new.insert(old, ObjectRef::new(new.number, 0));
            handles.push((new, handle));
        }
        let removed_refs: BTreeSet<ObjectRef> = skip_refs.iter().copied().collect();
        let map = |object_ref: ObjectRef| {
            if options.qdf {
                qdf_emission_renumber
                    .get(&object_ref)
                    .copied()
                    // cov:ignore-start: ObjStm members are selected from the same complete QDF emission map
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "full-rewrite: QDF ObjStm reference {object_ref} absent from emission map"
                        ))
                    })
                // cov:ignore-end
            } else {
                renumber.new_for_original(object_ref).ok_or_else(|| {
                    // cov:ignore-start: ObjStm members are selected from the same complete renumber map
                    crate::Error::Unsupported(format!(
                        "full-rewrite: ObjStm reference {object_ref} absent from renumber map"
                    ))
                    // cov:ignore-end
                }) // cov:ignore: ObjStm members are selected from the same complete renumber map
            }
        };
        let mut qdf_first_member_body_offset = None;
        let mut qdf_marker_starts = Vec::new();
        let mut qdf_marker_lengths = Vec::new();
        let emit_objstm_body = if options.qdf {
            object_streams::emit_objstm_body_from_handles_with_writer_qdf
        } else {
            object_streams::emit_objstm_body_from_handles_with_writer
        };
        let mut body = emit_objstm_body(&handles, &mut |out, member_index, member_ref, handle| {
            if options.qdf {
                let marker_start = out.len();
                out.extend_from_slice(
                    format!(
                        "%% Object stream: object {}, index {}",
                        member_ref.number, member_index
                    )
                    .as_bytes(),
                );
                if !options.no_original_object_ids {
                    if let Some(original) = handle.object_ref() {
                        out.extend_from_slice(
                            format!("; original object ID: {}", original.number).as_bytes(),
                        );
                        // cov:ignore-start: PDF object-stream members have generation zero in qpdf
                        if original.generation != 0 {
                            out.extend_from_slice(format!(" {}", original.generation).as_bytes());
                        }
                        // cov:ignore-end
                    } // cov:ignore: every ObjStm member is an indirect source object
                }
                out.push(b'\n');
                // qpdf's `/First` includes the object-stream marker comment
                // but excludes the optional `%% Page N` context comment:
                // QPDFWriter records the pair-table offset immediately before
                // entering `writeObject` (QPDFWriter.cc:1773-1800).
                qdf_first_member_body_offset.get_or_insert(out.len());
                qdf_marker_starts.push(marker_start);
                qdf_marker_lengths.push(out.len() - marker_start);
            }
            let result = if options.qdf {
                if let Some(original) = handle.object_ref() {
                    if let Some(&seq) = page_seq.get(&original) {
                        out.extend_from_slice(format!("%% Page {seq}\n").as_bytes());
                    }
                } // cov:ignore: every ObjStm member has a source ObjectRef
                handle.write_object_qdf_with_ref_map_and_removed(out, 0, &map, &removed_refs)
            } else {
                handle.write_object_with_ref_map_and_removed(out, &map, &removed_refs)
            }; // cov:ignore: llvm-cov maps the successful callback branch closing here
            if result.is_ok() {
                report_progress_event(options)?;
            } // cov:ignore: llvm-cov maps the successful progress branch closing here
            result
        })?; // cov:ignore: handle-native ObjStm member emission; LLVM maps the call continuation here
        if options.qdf {
            // QPDF records each pair-table offset after that member's marker
            // comment, while `/First` starts after the first marker only. The
            // handle emitter records marker starts so we can reproduce qpdf's
            // two-pass offsets without a second legacy serialization route.
            let first_marker_len = qdf_marker_lengths.first().copied().ok_or_else(|| {
                // cov:ignore-start: a non-empty Generate batch invokes the marker callback once per member
                crate::Error::Internal("QDF ObjStm marker lengths are empty".to_string())
                // cov:ignore-end
            })?; // cov:ignore: a non-empty Generate batch always records its first marker
            let objects_section = body.bytes.split_off(body.first_offset);
            let mut pair_table = Vec::new();
            for (index, ((new_ref, _), (&marker_start, &marker_len))) in handles
                .iter()
                .zip(qdf_marker_starts.iter().zip(qdf_marker_lengths.iter()))
                .enumerate()
            {
                if index != 0 {
                    pair_table.push(b'\n');
                }
                let offset = marker_start
                    .checked_add(marker_len)
                    .and_then(|end| end.checked_sub(first_marker_len))
                    .ok_or_else(|| {
                        // cov:ignore-start: marker offsets are lengths of one in-memory Vec and cannot overflow
                        crate::Error::Unsupported(
                            "QDF ObjStm member offset overflows usize".to_string(),
                        )
                        // cov:ignore-end
                    })?; // cov:ignore: marker arithmetic cannot overflow an in-memory Vec
                let _ = write!(pair_table, "{} {}", new_ref.number, offset);
            }
            pair_table.push(b'\n');
            body.first_offset = pair_table.len();
            pair_table.extend_from_slice(&objects_section);
            body.bytes = pair_table;
        }
        let objstm_compression = if options.qdf {
            CompressStreams::No
        } else {
            options.compress_streams
        };
        let extends = if let Some(source_container) = source_container_for_batch[batch_idx] {
            let source_handle = pdf.get_object_handle(source_container);
            pdf.resolve(&source_handle)?;
            let extends = source_handle
                .as_stream_dict()
                .map(|dict| dict.try_get_key(b"/Extends"))
                .transpose()?
                .and_then(|handle| handle.object_ref());
            extends.map(|extends| {
                remap_source_objstm_extends(
                    extends,
                    &source_container_to_batch,
                    &container_refs,
                    options.qdf,
                    &qdf_emission_renumber,
                    &renumber,
                )
            })
        } else {
            None
        }
        .flatten();
        let (stream_handle, stream_data) =
            object_streams::wrap_objstm_body_as_handle(&body, objstm_compression, extends)?;
        let objstm_first = if options.qdf {
            body.first_offset
                .checked_add(qdf_first_member_body_offset.unwrap_or(0))
                // cov:ignore-start: an allocatable ObjStm body cannot overflow usize
                .ok_or_else(|| {
                    crate::Error::Unsupported("QDF ObjStm /First overflows usize".to_string())
                })?
            // cov:ignore-end
        } else {
            body.first_offset
        };
        let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
            // cov:ignore-start: wrap_objstm_body_as_handle constructs a stream unconditionally
            crate::Error::Internal("ObjStm handle lost its stream dictionary".to_string())
            // cov:ignore-end
        })?; // cov:ignore: wrap_objstm_body_as_handle constructs a stream unconditionally
        let mut stream_length = stream_data.len();
        if let Some(ctx) = &encrypt_ctx {
            adjust_aes_stream_length(&mut stream_length, ctx, true)?;
        }
        stream_dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(stream_length).map_err(|_| {
                // cov:ignore-start: an allocatable ObjStm payload fits in i64
                crate::Error::Unsupported(
                    "encrypted ObjStm /Length does not fit in i64".to_string(),
                )
                // cov:ignore-end
            })?), // cov:ignore: an allocatable ObjStm payload fits in i64
        )?; // cov:ignore: validated ObjStm /Length replacement; LLVM maps the call continuation here

        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", container_ref.number).as_bytes());
        // Encrypt the ObjStm container as a single blob (PDF 1.7 §7.5.7).
        // Member objects' strings are NOT individually encrypted; the container
        // stream's encryption covers them all.
        let identity_map = |object_ref: ObjectRef| Ok(object_ref);
        let no_removed_refs = BTreeSet::new();
        if let Some(ctx) = &encrypt_ctx {
            if let Some(emitter) = encrypted_strings.as_mut() {
                emitter.write_handle_stream_dict_with_ref_map(
                    &mut bytes,
                    container_ref,
                    None,
                    &stream_dict,
                    encrypted_strings::StreamDictOptions::new(false, false, true),
                    &identity_map,
                    &no_removed_refs,
                    None,
                )?; // cov:ignore: encrypted ObjStm dictionary route; LLVM maps the call continuation here
            } else {
                // cov:ignore-start: encrypted output always constructs the handle-aware emitter
                stream_dict.write_stream_body_with_ref_map_and_removed(
                    &mut bytes,
                    false,
                    &identity_map,
                    &no_removed_refs,
                )?;
                // cov:ignore-end
            }
            write_stream_payload_with_pipeline(
                &mut bytes,
                &stream_data,
                options.newline_before_endstream,
                container_ref,
                ctx,
                true,
                None,
            )?; // cov:ignore: the encrypted ObjStm route executes; this call continuation has no counter.
        } else if options.qdf {
            bytes.extend_from_slice(b"<<\n  /Type /ObjStm\n");
            bytes.extend_from_slice(format!("  /Length {stream_length}\n").as_bytes());
            bytes.extend_from_slice(format!("  /N {}\n", body.n_members).as_bytes());
            bytes.extend_from_slice(format!("  /First {objstm_first}\n").as_bytes());
            if let Some(extends) = extends {
                bytes.extend_from_slice(
                    format!("  /Extends {} {} R\n", extends.number, extends.generation).as_bytes(),
                );
            }
            bytes.extend_from_slice(b">>");
            serialize::write_stream_payload_with_qdf(
                &mut bytes,
                &stream_data,
                options.newline_before_endstream,
                true,
            );
        } else {
            stream_dict.write_stream_body_with_ref_map_and_removed(
                &mut bytes,
                false,
                &identity_map,
                &no_removed_refs,
            )?; // cov:ignore: plain ObjStm payload route; LLVM maps the call continuation here
            serialize::write_stream_payload(
                &mut bytes,
                &stream_data,
                options.newline_before_endstream,
            );
        }
        bytes.extend_from_slice(b"\nendobj\n");
        // QDF inter-object blank-line separator (flpdf-9hc.6.10). This applies
        // to ordinary emitted objects; ObjStm member bodies use their own qpdf
        // pair-table and member framing.
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(container_ref.number, (0, emit_offset));
        if options.qdf {
            qdf_container_chunks.insert(batch_idx, (emit_offset, bytes.len()));
        }
    }

    // qpdf's standard enqueue walk interleaves ObjStm containers with ordinary
    // objects: a container is written when the first member in its group is
    // reached, and its sorted members receive numbers immediately. The
    // coordinator materializes ordinary and container chunks separately so it
    // can merge them in that same order and repair every xref offset.
    if options.qdf && !plan.batches.is_empty() {
        let original_bytes = bytes.clone();
        let mut merged_body = Vec::new();
        let mut chunk_transforms = Vec::<(usize, usize, usize)>::new();
        let mut appended_batches = BTreeSet::new();
        let mut append_chunk = |old_start: usize, old_end: usize| {
            let new_start = qdf_body_start + merged_body.len();
            merged_body.extend_from_slice(&original_bytes[old_start..old_end]);
            chunk_transforms.push((old_start, old_end, new_start));
        };

        for (_, old_ref) in &renumbered {
            if let Some(&batch_idx) = member_batch_index.get(old_ref) {
                if appended_batches.insert(batch_idx) {
                    if let Some(&(start, end)) = qdf_container_chunks.get(&batch_idx) {
                        append_chunk(start, end);
                    }
                }
            } else if let Some(&(start, end)) = qdf_main_chunks.get(old_ref) {
                append_chunk(start, end);
            }
        }
        // Every planned group is reachable by construction. Keep this
        // defensive completion for malformed graphs so the output remains
        // structurally complete rather than silently dropping a container.
        for batch_idx in 0..plan.batches.len() {
            // cov:ignore-start: every planned Generate batch is emitted before merge
            if appended_batches.insert(batch_idx) {
                let &(start, end) = qdf_container_chunks.get(&batch_idx).ok_or_else(|| {
                    crate::Error::Internal(
                        "QDF ObjStm container chunk missing during merge".to_string(),
                    )
                })?;
                append_chunk(start, end);
            }
            // cov:ignore-end
        }

        bytes.truncate(qdf_body_start);
        bytes.extend_from_slice(&merged_body);
        for (_, offset) in offsets.values_mut() {
            let old_offset = *offset;
            let Some(&(old_start, _, new_start)) = chunk_transforms
                .iter()
                .find(|(start, end, _)| old_offset >= *start && old_offset < *end)
            else {
                // cov:ignore-start: every recorded body offset belongs to one emitted merge chunk
                return Err(crate::Error::Internal(
                    "QDF body offset missing during ObjStm merge".to_string(),
                ));
                // cov:ignore-end
            };
            *offset = new_start + (old_offset - old_start);
        }
    }

    // ── flpdf-9hc.4.9: emit the /Encrypt dictionary as a plaintext indirect
    // object. Per PDF 1.7 §7.6.1 the /Encrypt dict itself is never encrypted;
    // its strings (/U /O /UE /OE /Perms) are already in their final wire form
    // from the dict builders.
    if let Some(ctx) = &encrypt_ctx {
        let emit_offset = bytes.len();
        bytes.extend_from_slice(format!("{} 0 obj\n", ctx.encrypt_ref.number).as_bytes());
        let encrypt_handle = ctx.encrypt_dict_handle();
        encrypted_strings::write_encryption_dictionary_handle(&mut bytes, &encrypt_handle)?;
        bytes.extend_from_slice(b"\nendobj\n");
        if options.qdf {
            bytes.push(b'\n');
        }
        offsets.insert(ctx.encrypt_ref.number, (0, emit_offset));
    }

    // Build xref / trailer matching the input's xref form.
    let xref_offset = bytes.len();
    // `object_count` is the smallest object number strictly greater than every
    // emitted one — i.e. the number we'll assign to a freshly created xref
    // stream object.  Using `saturating_add` here would silently fail when the
    // input's highest object number is `u32::MAX`: we'd reuse that exact
    // number for the xref stream and collide with an existing object.  Use
    // `checked_add` so the overflow surfaces as an explicit error instead.
    let max_object_number = offsets.keys().next_back().copied().unwrap_or(0);
    let object_count: usize = max_object_number
        .checked_add(1)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| {
            crate::Error::Unsupported("full-rewrite: object count does not fit in u32".to_string())
        })?;

    let mut written_xref = BTreeMap::<ObjectRef, XrefEntry>::new();
    match effective_xref_form {
        XrefForm::Table => {
            // Classic xref table.
            bytes.extend_from_slice(format!("xref\n0 {}\n", object_count).as_bytes());
            bytes.extend_from_slice(b"0000000000 65535 f \n");
            for number in 1..object_count {
                match offsets.get(&(number as u32)) {
                    Some((generation, offset)) => bytes
                        .extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes()),
                    None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
                }
            }
            for number in 1..object_count {
                let object_number = number as u32;
                if let Some(&(_generation, offset)) = offsets.get(&object_number) {
                    written_xref.insert(
                        ObjectRef::new(object_number, 0),
                        XrefEntry::Uncompressed {
                            // cov:ignore-start: offsets originate in Vec::len and usize fits u64
                            // on every supported target.
                            offset: u64::try_from(offset).map_err(|_| {
                                crate::Error::Unsupported(
                                    "xref offset does not fit u64".to_string(),
                                )
                            })?,
                            // cov:ignore-end
                        },
                    );
                } // cov:ignore: LLVM maps the covered contiguous-xref branch exit to this brace
            }

            // Trailer — start from the document trailer, strip incremental keys.
            let trailer = build_writer_trailer_handle(
                pdf,
                object_count,
                new_root,
                root_handle.as_ref(),
                options,
                encrypt_ctx.as_ref(),
                options.deterministic_id,
                generated_id.as_ref(),
            )?; // cov:ignore: validated writer trailer construction; LLVM maps this continuation to the call setup
            let trailer_map = |object_ref: ObjectRef| {
                if options.qdf {
                    qdf_emission_renumber
                        .get(&object_ref)
                        .copied()
                        .ok_or_else(|| {
                            // cov:ignore-start: catalog-first planning inserts every live QDF trailer reference
                            crate::Error::Unsupported(format!(
                                "full-rewrite: QDF trailer reference {object_ref} absent from emission map"
                            ))
                            // cov:ignore-end
                        }) // cov:ignore: catalog-first planning makes every QDF trailer reference resolvable
                } else {
                    renumber.new_for_original(object_ref).ok_or_else(|| {
                        // cov:ignore-start: catalog-first planning inserts every live trailer reference
                        crate::Error::Unsupported(format!(
                            "full-rewrite: trailer reference {object_ref} absent from renumber map"
                        ))
                        // cov:ignore-end
                    }) // cov:ignore: catalog-first planning makes every trailer reference resolvable
                }
            };

            if options.qdf {
                // qpdf --qdf trailer: "trailer <<" on one line, then one
                // "  /Key value" entry per line with the keys alphabetically
                // sorted but /ID and /Encrypt forced last in that order
                // (verified against qpdf 11.9.0: minimal => /Root /Size /ID;
                // encrypted => /Info /Root /Size /ID /Encrypt, with the final
                // two entries on one line). Values use the EXISTING compact
                // serializer, which keeps the /ID array inline
                // ("[<hex><hex>]") — do NOT route the trailer through the qdf
                // dict serializer. Closing ">>" then startxref directly (no
                // extra leading newline) to match the qpdf reference.
                if options.deterministic_id {
                    let mut id_writer = |out: &mut Vec<u8>| {
                        write_deterministic_id_inline(
                            out,
                            &det_id_info_suffix,
                            det_id_source_id0.as_deref(),
                        )
                    };
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.write_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        true,
                        Some(&mut id_writer),
                        &trailer_map,
                        &skip_ref_set,
                        suppress_null_values,
                    )?;
                    // cov:ignore-end
                } else {
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.write_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        true,
                        None,
                        &trailer_map,
                        &skip_ref_set,
                        suppress_null_values,
                    )?;
                    // cov:ignore-end
                }
                bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
            } else {
                // qpdf classic trailer: the dict sits on the `trailer ` line
                // (single space, not its own line) with keys sorted but /ID
                // forced last — `trailer << /Info .. /Root .. /Size N /ID [..]
                // >>` (verified against qpdf 11.9.0 static-id goldens).
                if options.deterministic_id {
                    let mut id_writer = |out: &mut Vec<u8>| {
                        write_deterministic_id_inline(
                            out,
                            &det_id_info_suffix,
                            det_id_source_id0.as_deref(),
                        )
                    };
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.write_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        false,
                        Some(&mut id_writer),
                        &trailer_map,
                        &skip_ref_set,
                        suppress_null_values,
                    )?;
                    // cov:ignore-end
                } else {
                    // cov:ignore-start: multiline handle-native trailer call; branch selection is covered by the writer fixtures
                    trailer.write_trailer_with_ref_map(
                        &mut bytes,
                        false,
                        false,
                        None,
                        &trailer_map,
                        &skip_ref_set,
                        suppress_null_values,
                    )?;
                    // cov:ignore-end
                }
                bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
            }
        }

        XrefForm::Stream => {
            // Cross-reference stream emission is delegated to the canonical
            // plain-writer xref layer below.
            // cov:ignore-start: object_count is bounded by the u32 object-number
            // space before this branch, so this usize overflow requires an
            // unallocatable PDF-sized object universe.
            let xref_size = object_count.checked_add(1).ok_or_else(|| {
                crate::Error::Unsupported("full-rewrite: xref-stream /Size overflows usize".into())
            })?;
            // cov:ignore-end
            let old_to_new: HashMap<ObjectRef, ObjectRef> = if options.qdf {
                qdf_emission_renumber.clone()
            } else {
                renumbered
                    .iter()
                    .map(|(new_ref, old_ref)| (*old_ref, *new_ref))
                    .collect()
            };
            let layout = plain::xref::BodyLayout {
                uncompressed: offsets.clone(),
                compressed: member_new_to_batch
                    .iter()
                    .map(|(&number, &(container, index))| {
                        (number, plain::xref::CompressedLocation { container, index })
                    })
                    .collect(),
            };
            let trailer_handle = build_writer_trailer_handle(
                pdf,
                xref_size,
                new_root,
                root_handle.as_ref(),
                options,
                encrypt_ctx.as_ref(),
                options.deterministic_id,
                generated_id.as_ref(),
            )?; // cov:ignore: validated xref trailer construction; LLVM maps this continuation to the call setup
            let id = if options.deterministic_id {
                plain::xref::IdPlan::Deterministic {
                    source_id0: det_id_source_id0.clone(),
                    info_suffix: det_id_info_suffix.clone(),
                }
            } else {
                plain::xref::IdPlan::Materialized {
                    value: plain::xref::materialized_id_handle(
                        &trailer_handle.try_get_key(b"/ID")?,
                    )?, // cov:ignore: build_writer_trailer_handle constructs the writer-owned /ID in the validated two-string shape
                }
            };
            let trailer_map = |object_ref: ObjectRef| {
                old_to_new.get(&object_ref).copied().ok_or_else(|| {
                    // cov:ignore-start: the direct Catalog is collected by the
                    // same canonical traversal that builds `old_to_new`, so a
                    // live reference can never be absent here.
                    crate::Error::Unsupported(format!(
                        "full-rewrite: direct /Root reference {object_ref} absent from renumber map"
                    ))
                    // cov:ignore-end
                }) // cov:ignore: the direct-root reference map is exercised; LLVM places the successful closure-exit counter on this continuation line.
            };
            let direct_root = if new_root.is_none() {
                let root = trailer_handle.try_get_key(b"/Root")?;
                let mut direct_root = Vec::new();
                if options.qdf {
                    root.write_object_qdf_with_ref_map_and_removed(
                        &mut direct_root,
                        0,
                        &trailer_map,
                        &skip_ref_set,
                    )?; // cov:ignore: the canonical direct-root serializer is exercised; LLVM maps this call terminator to a zero-count continuation region.
                } else {
                    root.write_object_with_ref_map_and_removed(
                        &mut direct_root,
                        &trailer_map,
                        &skip_ref_set,
                    )?; // cov:ignore: the canonical direct-root serializer is exercised; LLVM maps this call terminator to a zero-count continuation region.
                }
                Some(direct_root)
            } else {
                None
            };
            let trailer = plain::xref::TrailerPlan {
                form: XrefForm::Stream,
                canonical_entries: plain::plan::canonical_trailer_entries_with_visibility(
                    pdf,
                    &old_to_new,
                    &skip_ref_set,
                    suppress_null_values,
                )?, // cov:ignore: live trailer references are validated by the canonical map
                root: new_root,
                direct_root,
                id,
                encrypt: encrypt_ctx.as_ref().map(|ctx| ctx.encrypt_ref),
                structural_filtered: !options.qdf
                    && matches!(options.compress_streams, CompressStreams::Yes),
                qdf: options.qdf,
            };
            written_xref = plain::xref::append_xref_and_trailer(&mut bytes, &layout, &trailer)?;
        }
    }

    out.write_all(&bytes)?;
    Ok(WriterResult::new(emitted_old_to_new, written_xref))
}

/// Apply the stream compression policy to a single stream object.
///
/// This is the choke-point for re-emitting **regular indirect stream
/// objects** in the canonical rewrite path. The cross-reference stream and
/// object-stream (ObjStm) containers apply the same `CompressStreams`
/// policy on their own dedicated branches (the xref-stream branch below
/// and `object_streams::wrap_objstm_body`); they do not flow through
/// this function. QDF mode is exempt because it has its own stream framing and
/// decode policy.
///
/// # Policy: `CompressStreams::Yes` (default)
///
/// Decode the stream through its declared filter pipeline and re-encode with a
/// single `/FlateDecode` filter.  This matches qpdf's default passthrough mode.
///
/// Streams whose decode succeeds but re-encode fails (vanishingly rare for
/// in-memory zlib) are returned verbatim.
///
/// # Policy: `CompressStreams::No`
///
/// Decode the stream and emit the raw bytes without any `/Filter`.  The
/// filter-related keys (`/Filter`, `/DecodeParms`, `/F`, `/FFilter`,
/// `/FDecodeParms`) are stripped from the output dictionary.
///
/// # Fallback for unsupported / corrupt inputs
///
/// When `decode_stream_data` returns an error — e.g. because the declared
/// filter is a lossy codec below `DecodeLevel::All`, an unsupported image
/// codec, or because the stream data is corrupt — the stream's `/Filter`
/// chain and data bytes are returned **verbatim**.  This preserves
/// readability: a PDF reader that understands the codec can still decode the
/// stream, and we do not corrupt the data by emitting uninterpreted bytes
/// under a wrong (or missing) filter declaration.  The one normalization
/// applied even on this path is `/Length`: qpdf writes every emitted stream's
/// `/Length` as a direct integer (the raw byte count), never an indirect
/// reference, so a source carrying `/Length M G R` has it directized to
/// `data.len()` here (the data bytes are untouched, so the value is unchanged
/// for a well-formed direct length).
///
/// # Byte-vs-observable note
///
/// For `CompressStreams::Yes`, flpdf's FlateDecode output uses
/// `flate2::Compression::default()`, which selects different compression
/// parameters than qpdf's internal zlib build.  The decoded bytes are
/// identical to qpdf's, but the raw compressed bytes differ.  This is
/// intentional: byte-identical agreement with qpdf is not a goal for this
/// toggle.
/// See [`CompressStreams`] for the full policy statement.
pub fn apply_stream_compress_policy(
    stream: &ObjectHandle,
    policy: CompressStreams,
) -> Result<ObjectHandle> {
    // This public helper predates PdfWriter's decode-level setting. Preserve
    // its contract of decoding every filter implemented by flpdf; only the
    // private PdfWriter bridge applies the configured qpdf decode-level gate.
    apply_stream_compress_policy_with_decode_level(stream, policy, DecodeLevel::All, false)
}

fn apply_stream_compress_policy_with_decode_level(
    stream: &ObjectHandle,
    policy: CompressStreams,
    decode_level: DecodeLevel,
    normalize_content: bool,
) -> Result<ObjectHandle> {
    let stream_dict = stream
        .as_stream_dict()
        .ok_or_else(|| Error::Unsupported("object is not a stream".to_owned()))?;
    let data = stream.get_raw_stream_data()?;
    let decodable =
        filter_chain_is_decodable(&stream_dict, policy, decode_level, normalize_content)?;
    if !decodable {
        let dict = stream_dictionary_copy(&stream_dict);
        dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
        )?;
        return Ok(ObjectHandle::stream(dict, data));
    }

    // A filter above the selected level has already returned through the raw
    // chain-preservation branch above, matching qpdf's all-or-nothing gate.
    // For an in-level chain, `decode_stream_data` is the single owner of
    // `/DecodeParms` shape alignment and per-filter parameter validation (the
    // responsibility corresponding to QPDF_Stream::filterable). Keep that
    // validation in the existing decoder instead of duplicating its parser
    // here; any resulting Err takes the raw-preservation fallback below.
    let decoded = match filters::decode_stream_data(&stream_dict, &data) {
        Ok(d) => d,
        Err(_) => {
            // Decode failure (unsupported codec or corrupt data): emit the data
            // and /Filter chain verbatim so downstream readers (e.g. image
            // renderers) can still interpret the stream correctly. qpdf, however,
            // writes EVERY emitted stream's /Length as a direct integer, never an
            // indirect reference; directize it here so a source carrying
            // `/Length M G R` does not leak an indirect /Length (and a renumbered
            // holder reference) into the output — a byte divergence from qpdf for
            // passthrough/non-decodable streams whose length holder is kept live
            // by another reference (flpdf-q1j2). The data bytes are untouched, so
            // /Length equals stream.data.len().
            let dict = stream_dictionary_copy(&stream_dict);
            dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(data.len()).unwrap_or(i64::MAX)),
            )?;
            return Ok(ObjectHandle::stream(dict, data));
        }
    };
    let decoded = if normalize_content {
        crate::normalize_content_stream(&decoded).into_bytes()
    } else {
        decoded
    };

    // Build a new dict: strip all filter-related keys, update /Length.
    // `/F` carries an external-file reference for the stream data, so we
    // strip it as well — otherwise readers may try to load the old external
    // file instead of the new embedded stream we just produced.
    let new_dict = stream_dictionary_copy(&stream_dict);
    new_dict.remove_key(b"/Filter");
    new_dict.remove_key(b"/DecodeParms");
    new_dict.remove_key(b"/F");
    new_dict.remove_key(b"/FFilter");
    new_dict.remove_key(b"/FDecodeParms");

    match policy {
        CompressStreams::Yes => {
            // Re-encode with a minimal FlateDecode dict.  If encoding fails
            // (vanishingly rare for in-memory zlib), keep the original stream
            // verbatim — declaring /FlateDecode on uncompressed bytes would
            // produce an unreadable PDF.
            let encode_dict = ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            )]);
            let encoded = match filters::encode_stream_data(&encode_dict, &decoded) {
                Ok(e) => e,
                Err(_) => return Ok(ObjectHandle::stream(new_dict, data)), // cov:ignore: in-memory Flate encoding failures are not injectable through supported writer input
            };

            // Always apply FlateDecode — even if the encoded result is larger
            // than the raw data (which can happen for small streams).  This
            // guarantees a single well-known filter regardless of stream size.
            new_dict.replace_key(b"/Filter", ObjectHandle::name(b"FlateDecode".to_vec()))?;
            new_dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
            )?;
            Ok(ObjectHandle::stream(new_dict, Rc::new(encoded)))
        }
        CompressStreams::No => {
            // Emit raw (decoded) bytes without any filter.
            new_dict.replace_key(
                b"/Length",
                ObjectHandle::integer(i64::try_from(decoded.len()).unwrap_or(i64::MAX)),
            )?;
            Ok(ObjectHandle::stream(new_dict, Rc::new(decoded)))
        }
    }
}

fn stream_dictionary_copy(dictionary: &ObjectHandle) -> ObjectHandle {
    ObjectHandle::dictionary(
        dictionary
            .as_dictionary()
            .unwrap_or_default()
            .into_iter()
            .collect(),
    )
}

/// Apply qpdf's decode-level gate to the entire filter chain. qpdf does not
/// partially decode a chain: one filter above the selected level, or one
/// filter it cannot filter, makes the complete chain non-filterable.
///
/// `QPDF_Stream.cc:504-512,537-542` makes one important distinction: a
/// compress or content-normalization request supplies an encode flag, so
/// generalized filters are filterable even at decode level `none`; a plain
/// uncompress request at `none` preserves them. Specialized filters remain
/// gated by the selected level in every policy. Lossy `/DCTDecode` is admitted
/// only at `DecodeLevel::All`, matching `QPDF_Stream::pipeStreamData`.
fn filter_chain_is_decodable(
    dictionary: &ObjectHandle,
    policy: CompressStreams,
    decode_level: DecodeLevel,
    normalize_content: bool,
) -> Result<bool> {
    let filter = dictionary.try_get_key(b"/Filter")?;
    if filter.is_null() {
        return Ok(true);
    }
    let filters = if let Some(name) = filter.try_as_name()? {
        vec![ObjectHandle::name(name)]
    } else if let Some(filters) = filter.try_as_array()? {
        filters
    } else {
        return Ok(false);
    };

    filters.iter().try_fold(true, |allowed, filter| {
        let Some(name) = filter.try_as_name()? else {
            return Ok(false);
        };
        let name = match name.as_slice() {
            b"Fl" => b"FlateDecode".as_slice(),
            b"LZW" => b"LZWDecode".as_slice(),
            b"A85" => b"ASCII85Decode".as_slice(),
            b"AHx" => b"ASCIIHexDecode".as_slice(),
            b"RL" => b"RunLengthDecode".as_slice(),
            b"DCT" => b"DCTDecode".as_slice(),
            name => name,
        };
        match name {
            b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode" => Ok(allowed
                && (!matches!(decode_level, DecodeLevel::None)
                    || policy == CompressStreams::Yes
                    || normalize_content)),
            b"RunLengthDecode" => {
                Ok(allowed && matches!(decode_level, DecodeLevel::Specialized | DecodeLevel::All))
            }
            b"DCTDecode" => Ok(allowed && matches!(decode_level, DecodeLevel::All)),
            _ => Ok(false),
        }
    })
}

/// Collect the immediate `/Contents` containers that can hold direct streams.
/// Indirect streams are tracked separately by [`collect_content_stream_refs`].
/// The qpdf writer inspects the page value and one array level; it does not
/// follow flpdf-only reference-holder chains.
fn collect_content_container_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    containers: &mut BTreeSet<ObjectRef>,
) -> Result<()> {
    let page_handle = pdf.get_object_handle(page_ref);
    pdf.resolve(&page_handle)?;
    let contents = page_handle.try_get_key(b"/Contents")?;
    if contents.type_code()? == 10 {
        if contents.object_ref().is_none() {
            containers.insert(page_ref);
        }
        return Ok(());
    }

    if contents.try_as_array()?.is_none() {
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
pub(crate) fn collect_content_stream_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<Vec<ObjectRef>> {
    let page_handle = pdf.get_object_handle(page_ref);
    pdf.resolve(&page_handle)?;
    let contents = page_handle.try_get_key(b"/Contents")?;
    if contents.type_code()? == 10 {
        return Ok(contents.object_ref().into_iter().collect());
    }

    let Some(items) = contents.try_as_array()? else {
        return Ok(Vec::new());
    };
    let mut refs = Vec::with_capacity(items.len());
    for item in items {
        if item.type_code()? == 10 {
            if let Some(object_ref) = item.object_ref() {
                refs.push(object_ref);
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
    use std::io::Cursor;

    fn stream_with_filter(filter: Option<&[u8]>, data: Vec<u8>) -> ObjectHandle {
        let mut entries = vec![(
            b"/Length".to_vec(),
            ObjectHandle::integer(data.len() as i64),
        )];
        if let Some(filter) = filter {
            entries.push((b"/Filter".to_vec(), ObjectHandle::name(filter.to_vec())));
        }
        ObjectHandle::stream(ObjectHandle::dictionary(entries), Rc::new(data))
    }

    #[test]
    fn encryption_shape_reads_copy_encryption_handles() {
        let mut options = WriterOptions::default();
        options.copy_encryption = Some(CopyEncryptionSource {
            encrypt_dict: ObjectHandle::dictionary(vec![
                (b"/V".to_vec(), ObjectHandle::integer(4)),
                (b"/R".to_vec(), ObjectHandle::integer(4)),
            ]),
            file_key: vec![0; 16],
            id0: vec![0; 16],
            object_key_alg: ObjectKeyAlg::Rc4,
        });

        assert_eq!(encryption_shape(&options), Some((4, 4, true)));
    }

    #[test]
    fn stream_compression_policy_handles_public_and_private_routes() {
        let plain = stream_with_filter(None, b"q 1 0 cm\n".to_vec());
        let uncompressed = apply_stream_compress_policy(&plain, CompressStreams::No)
            .expect("an unfiltered stream can be emitted without compression");
        assert!(uncompressed
            .as_stream_dict()
            .expect("stream dictionary")
            .try_get_key(b"/Filter")
            .expect("filter lookup")
            .is_null());
        assert_eq!(
            uncompressed
                .get_raw_stream_data()
                .expect("stream data")
                .as_ref(),
            b"q 1 0 cm\n"
        );

        let compressed = apply_stream_compress_policy(&plain, CompressStreams::Yes)
            .expect("an unfiltered stream can be compressed");
        assert_eq!(
            compressed
                .as_stream_dict()
                .expect("stream dictionary")
                .try_get_key(b"/Filter")
                .expect("filter lookup")
                .as_name(),
            Some(b"FlateDecode".to_vec())
        );
        assert_eq!(
            filters::decode_stream_data(
                &compressed.as_stream_dict().expect("stream dictionary"),
                &compressed.get_raw_stream_data().expect("stream data"),
            )
            .expect("compressed payload decodes"),
            b"q 1 0 cm\n"
        );

        let flate_dictionary = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::name(b"FlateDecode".to_vec()),
            ),
            (b"/Length".to_vec(), ObjectHandle::integer(8)),
        ]);
        let encoded =
            filters::encode_stream_data(&flate_dictionary, b"decoded").expect("flate encoder");
        let flate = ObjectHandle::stream(flate_dictionary, Rc::new(encoded));
        let preserved = apply_stream_compress_policy_with_decode_level(
            &flate,
            CompressStreams::No,
            DecodeLevel::None,
            false,
        )
        .expect("decode-level none preserves a gated filter chain");
        assert_eq!(
            preserved
                .as_stream_dict()
                .expect("stream dictionary")
                .try_get_key(b"/Filter")
                .expect("filter lookup")
                .as_name(),
            Some(b"FlateDecode".to_vec())
        );

        let corrupt = stream_with_filter(Some(b"FlateDecode"), b"not-flate".to_vec());
        let passthrough = apply_stream_compress_policy_with_decode_level(
            &corrupt,
            CompressStreams::No,
            DecodeLevel::All,
            false,
        )
        .expect("corrupt data follows qpdf's raw-preservation path");
        assert_eq!(
            passthrough
                .get_raw_stream_data()
                .expect("raw stream data")
                .as_ref(),
            b"not-flate"
        );

        let normalized = apply_stream_compress_policy_with_decode_level(
            &plain,
            CompressStreams::No,
            DecodeLevel::None,
            true,
        )
        .expect("content normalization remains available on an unfiltered stream");
        assert!(!normalized
            .get_raw_stream_data()
            .expect("normalized stream data")
            .is_empty());
    }

    #[test]
    fn filter_chain_gate_covers_qpdf_filter_aliases_and_levels() {
        for name in [
            b"FlateDecode".as_slice(),
            b"Fl".as_slice(),
            b"LZWDecode".as_slice(),
            b"LZW".as_slice(),
            b"ASCII85Decode".as_slice(),
            b"A85".as_slice(),
            b"ASCIIHexDecode".as_slice(),
            b"AHx".as_slice(),
        ] {
            let dictionary = ObjectHandle::dictionary(vec![(
                b"/Filter".to_vec(),
                ObjectHandle::name(name.to_vec()),
            )]);
            assert!(filter_chain_is_decodable(
                &dictionary,
                CompressStreams::No,
                DecodeLevel::Generalized,
                false,
            )
            .expect("generalized filter gate"));
        }

        let run_length = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(b"RL".to_vec()),
        )]);
        assert!(!filter_chain_is_decodable(
            &run_length,
            CompressStreams::No,
            DecodeLevel::Generalized,
            false,
        )
        .expect("run-length generalized gate"));
        assert!(filter_chain_is_decodable(
            &run_length,
            CompressStreams::No,
            DecodeLevel::Specialized,
            false,
        )
        .expect("run-length specialized gate"));

        let dct = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(b"DCT".to_vec()),
        )]);
        assert!(!filter_chain_is_decodable(
            &dct,
            CompressStreams::No,
            DecodeLevel::Specialized,
            false,
        )
        .expect("DCT specialized gate"));
        assert!(
            filter_chain_is_decodable(&dct, CompressStreams::No, DecodeLevel::All, false,)
                .expect("DCT all gate")
        );

        let none = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(b"UnknownDecode".to_vec()),
        )]);
        assert!(
            !filter_chain_is_decodable(&none, CompressStreams::No, DecodeLevel::All, false,)
                .expect("unknown filter gate")
        );

        let malformed =
            ObjectHandle::dictionary(vec![(b"/Filter".to_vec(), ObjectHandle::integer(7))]);
        assert!(!filter_chain_is_decodable(
            &malformed,
            CompressStreams::No,
            DecodeLevel::All,
            false,
        )
        .expect("malformed filter gate"));

        let array = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::name(b"Fl".to_vec())]),
        )]);
        assert!(filter_chain_is_decodable(
            &array,
            CompressStreams::No,
            DecodeLevel::Generalized,
            false,
        )
        .expect("array filter gate"));
        let array_with_scalar = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(1)]),
        )]);
        assert!(!filter_chain_is_decodable(
            &array_with_scalar,
            CompressStreams::No,
            DecodeLevel::All,
            false,
        )
        .expect("array scalar filter gate"));

        let generalized = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        assert!(filter_chain_is_decodable(
            &generalized,
            CompressStreams::Yes,
            DecodeLevel::None,
            false,
        )
        .expect("compression enables generalized filtering at decode none"));
        assert!(filter_chain_is_decodable(
            &generalized,
            CompressStreams::No,
            DecodeLevel::None,
            true,
        )
        .expect("normalization enables generalized filtering at decode none"));
    }

    #[test]
    fn pclm_emits_a_synthetic_stream_for_a_page_xobject() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../tests/fixtures/compat/direct-root-one-page.pdf").to_vec(),
        ))
        .expect("fixture must open");
        let page = crate::pages::page_refs(&mut pdf).expect("page refs")[0];
        let page_handle = pdf.get_object_handle(page);
        pdf.resolve(&page_handle).expect("page resolves");
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

        let mut output = Vec::new();
        write_pclm(&mut pdf, &mut output, &WriterOptions::default()).expect("PCLm writer succeeds");
        assert!(output
            .windows(b"q /image Do Q\n".len())
            .any(|window| { window == b"q /image Do Q\n" }));
    }
}
