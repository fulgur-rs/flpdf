//! qpdf correspondence: `QPDFJob::addAttachments`, `QPDFJob::doListAttachments`, and `QPDFJob::doShowAttachment` (`libqpdf/QPDFJob.cc:876-927,2046-2087`).

use super::attachment_list::format_attachment_list_with_sink;
use super::lifecycle::{JobExitCode, QPDFJob};
use crate::filespec_helper::FileSpec;
use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
use crate::qpdf_time::default_pdf_date;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

struct PipelineHandleSink(PipelineHandle);

impl Pipeline for PipelineHandleSink {
    fn identifier(&self) -> &str {
        "qpdf job attachment save"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.0.finish()
    }
}

/// qpdf's per-file configuration for `QPDFJob::copyAttachments`.
///
/// `path` is retained only for the `copying attachments from PATH` verbose
/// diagnostic and the `file: PATH, key: ...` duplicate-key message; `source`
/// must already be opened (and authenticated) by the caller before being
/// passed to [`QPDFJob::copy_attachments`], matching this crate's existing
/// job boundary where document I/O stays a caller concern
/// (`QPDFJob::list_attachments`/`show_attachment` accept an already-open
/// [`Pdf`] the same way).
#[derive(Debug, Clone)]
pub struct AttachmentCopyOptions {
    /// Source PDF path, used only for diagnostics.
    pub path: PathBuf,
    /// Prefix prepended to each copied key.
    pub prefix: Vec<u8>,
    /// Emit qpdf's `copying attachments from ...` / `  key -> new_key` diagnostics.
    pub verbose: bool,
}

/// qpdf's per-file configuration for `QPDFJob::addAttachments`.
///
/// The path is retained by the provider-backed embedded-file stream; the
/// payload is not materialized by the job. `creation_date` and
/// `modification_date` carry raw PDF date strings so an explicit qpdf date is
/// preserved byte-for-byte. When omitted, the job supplies qpdf's process-stable
/// local-wall-clock date with its UTC offset (`QUtil::get_current_qpdf_time`
/// and `QPDFJob::AttConfig::endAddAttachment`, `libqpdf/QUtil.cc:867-934`,
/// `libqpdf/QPDFJob_config.cc:911-936`).
#[derive(Debug, Clone)]
pub struct AttachmentAddOptions {
    /// Path whose bytes are embedded.
    pub path: PathBuf,
    /// `/Names /EmbeddedFiles` name-tree key.
    pub key: Vec<u8>,
    /// Displayed `/F` and `/UF` filename.
    pub filename: Vec<u8>,
    /// Optional `/EmbeddedFile /Subtype` MIME name.
    pub mimetype: Option<Vec<u8>>,
    /// Optional `/Filespec /Desc` text.
    pub description: Option<Vec<u8>>,
    /// Optional raw `/Params /CreationDate` value.
    pub creation_date: Option<Vec<u8>>,
    /// Optional raw `/Params /ModDate` value.
    pub modification_date: Option<Vec<u8>>,
    /// Replace an existing entry with the same key.
    pub replace: bool,
    /// Emit qpdf's `attached ... with key ...` diagnostic.
    pub verbose: bool,
}

impl QPDFJob {
    /// Add one or more provider-backed attachments through the shared qpdf
    /// job lifecycle.
    ///
    /// This is the Rust translation of `QPDFJob::addAttachments`:
    /// `QPDFJob.cc:2037-2087`. Page mode is changed before duplicate
    /// detection, Filespec/EmbeddedFile objects are created through the
    /// typed helpers, and the canonical ObjectHandle name-tree route owns the
    /// insertion/replacement. A duplicate is reported only after all
    /// non-duplicate entries have been processed, matching qpdf's aggregate
    /// error boundary.
    pub fn add_attachments<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
        options: &[AttachmentAddOptions],
    ) -> Result<()> {
        // qpdf's sole caller only invokes `addAttachments` when
        // `attachments_to_add` is non-empty (`QPDFJob.cc:2241-2243`); an
        // empty batch is a no-op there, so no page-mode change happens.
        if options.is_empty() {
            return Ok(());
        }
        pdf.set_logger(self.logger());
        self.set_attachment_page_mode(pdf)?;

        let mut duplicated_keys = Vec::new();
        let default_date = default_pdf_date().to_vec();
        for option in options {
            let exists = {
                let mut embedded_files = pdf.embedded_files();
                embedded_files.get_embedded_file(&option.key)?.is_some()
            };
            if exists && !option.replace {
                duplicated_keys.push(option.key.clone());
                continue;
            }

            let filespec =
                FileSpec::create_file_spec_from_path(pdf, &option.filename, &option.path)?;
            {
                let mut filespec_helper = FileSpec::new(filespec.clone(), pdf)?;
                if let Some(description) = option.description.as_deref() {
                    filespec_helper.set_description(description)?;
                }
                let mut embedded_file = filespec_helper
                    .embedded_file()?
                    .expect("FileSpec::create_file_spec_from_path must create an /EmbeddedFile");
                let creation_date = option
                    .creation_date
                    .clone()
                    .unwrap_or_else(|| default_date.clone());
                let modification_date = option
                    .modification_date
                    .clone()
                    .unwrap_or_else(|| default_date.clone());
                embedded_file.set_creation_date(creation_date)?;
                embedded_file.set_mod_date(modification_date)?;
                if let Some(mimetype) = option.mimetype.as_deref() {
                    embedded_file.set_subtype(mimetype)?;
                }
            }

            pdf.embedded_files()
                .replace_embedded_file(&option.key, filespec)?;

            if option.verbose {
                // qpdf writes `filename`/`key` as raw bytes
                // (`QPDFJob.cc:2066-2068`); build the diagnostic as bytes
                // too so a non-UTF-8 value isn't replaced with U+FFFD.
                let mut message = Vec::new();
                message.extend_from_slice(self.message_prefix().as_bytes());
                message.extend_from_slice(b": attached ");
                message.extend_from_slice(&path_bytes(&option.path));
                message.extend_from_slice(b" as ");
                message.extend_from_slice(&option.filename);
                message.extend_from_slice(b" with key ");
                message.extend_from_slice(&option.key);
                message.push(b'\n');
                self.logger().info(message)?;
            }
        }

        if duplicated_keys.is_empty() {
            return Ok(());
        }

        let keys = duplicated_keys
            .iter()
            .map(|key| String::from_utf8_lossy(key).into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        Err(Error::System(format!(
            "{} already has attachments with the following keys: {}; use --replace to replace or --key to specify a different key",
            self.input_name(), keys
        )))
    }

    /// Add one attachment through [`Self::add_attachments`].
    pub fn add_attachment<R: Read + Seek + 'static>(
        &mut self,
        pdf: &mut Pdf<R>,
        options: AttachmentAddOptions,
    ) -> Result<()> {
        self.add_attachments(pdf, std::slice::from_ref(&options))
    }

    /// Copy every embedded file from `source` into `target` through the
    /// shared qpdf job lifecycle.
    ///
    /// This is the Rust translation of `QPDFJob::copyAttachments`:
    /// `QPDFJob.cc:2089-2135`. Page mode is changed before duplicate
    /// detection; each source filespec's object graph is copied through the
    /// canonical [`Pdf::copy_foreign_object`] cross-document primitive and
    /// inserted into the target's `/EmbeddedFiles` name tree; a duplicate
    /// key is reported only after every entry has been processed, matching
    /// qpdf's aggregate error boundary. Warnings observed on `source` (e.g.
    /// from a repaired open) are folded into this job's own warning state,
    /// matching qpdf's `other->anyWarnings()` check (`QPDFJob.cc:2116-2118`).
    ///
    pub fn copy_attachments<R1, R2>(
        &mut self,
        target: &mut Pdf<R1>,
        source: &mut Pdf<R2>,
        options: &AttachmentCopyOptions,
    ) -> Result<()>
    where
        R1: Read + Seek + 'static,
        R2: Read + Seek + 'static,
    {
        target.set_logger(self.logger());
        source.set_logger(self.logger());
        self.set_attachment_page_mode(target)?;

        if options.verbose {
            let mut message = Vec::new();
            message.extend_from_slice(self.message_prefix().as_bytes());
            message.extend_from_slice(b": copying attachments from ");
            message.extend_from_slice(options.path.display().to_string().as_bytes());
            message.push(b'\n');
            self.logger().info(message)?;
        }

        let other_attachments = source.embedded_files().get_embedded_files()?;
        let mut duplicates: Vec<String> = Vec::new();
        for (key, filespec) in other_attachments {
            let mut new_key = options.prefix.clone();
            new_key.extend_from_slice(&key);

            let exists = target
                .embedded_files()
                .get_embedded_file(&new_key)?
                .is_some();
            if exists {
                duplicates.push(format!(
                    "file: {}, key: {}",
                    options.path.display(),
                    String::from_utf8_lossy(&new_key)
                ));
                continue;
            }

            let copied = target.copy_foreign_object(&filespec)?;
            target
                .embedded_files()
                .replace_embedded_file(&new_key, copied)?;

            if options.verbose {
                let mut message = Vec::new();
                message.extend_from_slice(b"  ");
                message.extend_from_slice(&key);
                message.extend_from_slice(b" -> ");
                message.extend_from_slice(&new_key);
                message.push(b'\n');
                self.logger().info(message)?;
            }
        }

        self.record_document_warnings(source);

        if duplicates.is_empty() {
            return Ok(());
        }

        Err(Error::System(format!(
            "{} already has attachments with keys that conflict with attachments from other files: {}. Use --prefix with --copy-attachments-from or manually copy individual attachments.",
            self.input_name(),
            duplicates.join("; ")
        )))
    }

    fn set_attachment_page_mode<R: Read + Seek>(&self, pdf: &mut Pdf<R>) -> Result<()> {
        // qpdf's `maybe_set_pagemode` (`QPDFJob.cc:2036-2042`) calls
        // `QPDF::getRoot`, which throws when the trailer has no valid
        // `/Root` dictionary (`QPDF.cc:2355-2359`).
        let root_ref = pdf.root_ref().ok_or(Error::Missing("/Root"))?;
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root)?;
        if root.try_get_key(b"/PageMode")?.try_is_null()? {
            root.replace_key(b"/PageMode", ObjectHandle::name(b"UseAttachments".to_vec()))?;
            pdf.mark_object_handle_dirty(&root)?;
        }
        Ok(())
    }

    /// List embedded files through the shared qpdf info pipeline.
    ///
    /// `QPDFJob::doListAttachments` owns the output and warning lifecycle,
    /// while `QPDFEmbeddedFileDocumentHelper` and the FileSpec/EF helpers own
    /// name-tree traversal and metadata projection. The existing
    /// [`format_attachment_list_with_sink`] implementation remains the one
    /// attachment traversal route.
    pub fn list_attachments<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        verbose: bool,
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        let input_name = self.input_name_bytes().to_owned();
        self.inspect(pdf, |pdf| {
            emit_list_attachments(pdf, &logger, &input_name, verbose)
        })
    }

    /// Emit the attachment list without completing the enclosing job.
    pub(crate) fn list_attachments_report<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        verbose: bool,
    ) -> Result<()> {
        let logger = self.logger();
        let input_name = self.input_name_bytes().to_owned();
        emit_list_attachments(pdf, &logger, &input_name, verbose)
    }

    /// Show one embedded file through the shared qpdf save pipeline.
    ///
    /// The attachment is resolved before `save_to_standard_output`, matching
    /// qpdf's `doShowAttachment` order: a missing key is a fatal inspection
    /// error and does not claim the standard-output save route
    /// (`libqpdf/QPDFJob.cc:916-927`).
    pub fn show_attachment<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &[u8],
    ) -> Result<JobExitCode> {
        let logger = self.logger();
        self.inspect(pdf, |pdf| emit_show_attachment(pdf, &logger, key))
    }

    /// Emit one embedded file without completing the enclosing job.
    pub(crate) fn show_attachment_report<R: Read + Seek>(
        &self,
        pdf: &mut Pdf<R>,
        key: &[u8],
    ) -> Result<()> {
        let logger = self.logger();
        emit_show_attachment(pdf, &logger, key)
    }
}

fn emit_list_attachments<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    logger: &crate::QPDFLogger,
    input_name: &[u8],
    verbose: bool,
) -> Result<()> {
    let listing = format_attachment_list_with_sink(pdf, verbose, |data| logger.info(data))?;
    if listing.is_none() {
        let mut message = input_name.to_vec();
        message.extend_from_slice(b" has no embedded files\n");
        logger.info(message)?;
    }
    Ok(())
}

fn emit_show_attachment<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    logger: &crate::QPDFLogger,
    key: &[u8],
) -> Result<()> {
    let filespec = {
        let mut embedded_files = pdf.embedded_files();
        embedded_files.get_embedded_file(key)?
    }
    .ok_or_else(|| raw_attachment_error(key, b" not found"))?;
    let mut filespec = FileSpec::new(filespec, pdf)?;
    let embedded_file = filespec
        .embedded_file()?
        .ok_or_else(|| raw_attachment_error(key, b" has no resolvable /EmbeddedFile stream"))?;
    logger.save_to_standard_output(true)?;
    let save = logger.get_save()?;
    let mut sink = PipelineHandleSink(save);
    let _ = embedded_file.pipe_stream_data(&mut sink)?;
    Ok(())
}

fn raw_attachment_error(key: &[u8], suffix: &[u8]) -> Error {
    let mut message = b"unsupported PDF feature: attachment ".to_vec();
    message.extend_from_slice(key);
    message.extend_from_slice(suffix);
    Error::SystemBytes(message)
}

/// This is a convenience wrapper around [`FileSpec::create_file_spec_from_path`] +
/// [`crate::embedded_files::insert_embedded_file`] that:
///
/// 1. Streams the file at `path` through the deferred provider factory.
/// 2. Derives the name-tree key and `/F`/`/UF` filename from the path's
///    **basename** (the last component of the path).
/// 3. Builds a `/Filespec` + `/EmbeddedFile` pair without installing a local
///    filter. `/Params /Size` and `/Params /CheckSum` reflect the **raw** bytes,
///    as required by ISO 32000-1 §7.11.4.
/// 4. Inserts the pair into the catalog's `/Names /EmbeddedFiles` name tree
///    under the UTF-8 `key` (which may differ from the basename if the caller
///    wants an explicit tree key).
///
/// Returns the [`ObjectRef`] of the newly created `/Filespec` dictionary.
///
/// # Parameters
///
/// - `pdf` — the target document (must be mutable).
/// - `key` — the name-tree key used to look up the attachment later (e.g. the
///   basename encoded as bytes, or any other agreed-upon string).
/// - `path` — path to the file on disk; its basename is used for `/F`/`/UF`.
///
/// # Errors
///
/// - [`Error::Io`] if the file cannot be opened or read.
/// - [`Error::Unsupported`] if the path has no basename or the basename is not
///   valid UTF-8.
/// - Any error from [`FileSpec::create_file_spec_from_path`] or
///   [`crate::embedded_files::insert_embedded_file`].
///
/// # Example
///
/// ```no_run
/// use std::io::Cursor;
/// use flpdf::Pdf;
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf: Pdf<Cursor<Vec<u8>>> = todo!();
/// let fs_ref = flpdf::add_attachment_from_path(
///     &mut pdf,
///     b"README.txt",
///     "/tmp/README.txt",
/// )?;
/// println!("inserted filespec at {fs_ref}");
/// # Ok(())
/// # }
/// ```
pub fn add_attachment_from_path<R, P>(pdf: &mut Pdf<R>, key: &[u8], path: P) -> Result<ObjectRef>
where
    R: Read + Seek,
    P: AsRef<Path>,
{
    let path = path.as_ref();

    // Derive the basename for /F and /UF.
    let basename = path
        .file_name()
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "add_attachment_from_path: path has no basename: {}",
                path.display()
            ))
        })?
        .to_str()
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "add_attachment_from_path: basename is not valid UTF-8: {}",
                path.display()
            ))
        })?;

    // Build the /Filespec + /EmbeddedFile through qpdf's path-provider route.
    // `create_file_spec` initially uses the same Unicode name for `/F` and
    // `/UF`; replace `/F` with the independent ASCII fallback while retaining
    // the original Unicode `/UF` value, matching FileSpecBuilder's behavior.
    let filespec_handle = FileSpec::create_file_spec_from_path(pdf, basename.as_bytes(), path)?;
    let filespec_ref = filespec_handle
        .object_ref()
        .expect("create_file_spec_from_path must create an indirect Filespec");
    let fallback = ascii_filename_fallback(basename);
    {
        let mut filespec = FileSpec::new(pdf.get_object_handle(filespec_ref), pdf)?;
        filespec.set_filename(basename.as_bytes(), Some(fallback.as_slice()))?;
    }
    crate::embedded_files::insert_embedded_file(pdf, key, filespec_ref)?;

    Ok(filespec_ref)
}

/// Return an ASCII-safe `/F` fallback while preserving readable ASCII filename parts.
pub fn ascii_filename_fallback(filename: &str) -> Vec<u8> {
    let fallback: String = filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if fallback.is_empty() || fallback.bytes().all(|b| b == b'.' || b == b'_') {
        b"attachment".to_vec()
    } else {
        fallback.into_bytes()
    }
}

// ── Attachment extraction API ─────────────────────────────────────────────────

/// Extract the decoded payload of an attachment identified by `key`.
///
/// Looks up `key` in the catalog's `/Names /EmbeddedFiles` name tree, resolves
/// the associated `/Filespec` dictionary, and decodes the `/EmbeddedFile` stream
/// (applying the filter chain, e.g. FlateDecode) to return the original file
/// contents.
///
/// # Note on direct-dict filespecs
///
/// Name-tree entries whose value is a direct `/Filespec` dictionary (rather than
/// an indirect reference) are not surfaced by the underlying
/// [`crate::embedded_files::list_embedded_files`] enumeration; they are
/// skipped with the same limitation documented there. Only attachments with
/// indirect-reference values are extractable by this function.
///
/// # Errors
///
/// - [`Error::Unsupported`] when `key` is not present in the name tree.  The
///   error message includes the missing key name and a sorted list of available
///   keys so the caller can emit an actionable diagnostic.
/// - [`Error::Unsupported`] when the filespec at `key` has no resolvable
///   `/EmbeddedFile` stream (e.g. the `/EF` sub-dictionary is absent or
///   malformed).
/// - Any error from [`Pdf::resolve`] or the filter decoder.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::Pdf;
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// let bytes = flpdf::extract_attachment(&mut pdf, b"report.pdf")?;
/// println!("extracted {} bytes", bytes.len());
/// # Ok(())
/// # }
/// ```
pub fn extract_attachment<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<Vec<u8>> {
    // Look up all entries in the name tree.
    let entries = crate::embedded_files::list_embedded_files(pdf)?;

    // Find the target key.
    let filespec_ref = match entries.iter().find(|(k, _)| k.as_slice() == key) {
        Some((_, r)) => *r,
        None => {
            // Collect available keys for an actionable error message.
            // Sorted so the diagnostic is deterministic / reproducible,
            // independent of name-tree iteration order (CodeRabbit nitpick).
            let mut available: Vec<String> = entries
                .iter()
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .collect();
            available.sort_unstable();
            let hint = if available.is_empty() {
                " (no attachments present)".to_string()
            } else {
                format!(" (available keys: {})", available.join(", "))
            };
            return Err(Error::Unsupported(format!(
                "extract_attachment: key {:?} not found{}",
                String::from_utf8_lossy(key),
                hint,
            )));
        }
    };

    // Resolve the filespec and decode its embedded file stream.
    let mut fs = FileSpec::new(pdf.get_object_handle(filespec_ref), pdf)?;
    let ef = fs.embedded_file()?.ok_or_else(|| {
        Error::Unsupported(format!(
            "extract_attachment: key {:?} has no resolvable /EmbeddedFile stream \
             (the /EF sub-dictionary may be absent or malformed)",
            String::from_utf8_lossy(key),
        ))
    })?;
    ef.payload()
}

/// Write the decoded payload of attachment `key` to `out`.
///
/// Decodes the embedded file stream via [`extract_attachment`] and writes all
/// bytes to `out` in a single [`Write::write_all`] call.
///
/// # Errors
///
/// Propagates all errors from [`extract_attachment`] and from `out.write_all`.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::Pdf;
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// let mut buf = Vec::new();
/// flpdf::write_attachment(&mut pdf, b"report.pdf", &mut buf)?;
/// println!("wrote {} bytes", buf.len());
/// # Ok(())
/// # }
/// ```
pub fn write_attachment<R: Read + Seek, W: Write>(
    pdf: &mut Pdf<R>,
    key: &[u8],
    out: &mut W,
) -> Result<()> {
    let bytes = extract_attachment(pdf, key)?;
    out.write_all(&bytes)?;
    Ok(())
}

/// Write the decoded payload of attachment `key` to a file at `path`.
///
/// Creates (or truncates) the file at `path` and writes the decoded stream
/// bytes.  This is the library-side counterpart of the CLI `-o` option
/// (wiring of the `-o` flag is handled by the CLI layer, not here).
///
/// # Errors
///
/// - Any error from [`extract_attachment`].
/// - [`Error::Io`] if the file cannot be created or written.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::Pdf;
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// flpdf::extract_attachment_to_path(&mut pdf, b"report.pdf", "/tmp/out.pdf")?;
/// # Ok(())
/// # }
/// ```
pub fn extract_attachment_to_path<R, P>(pdf: &mut Pdf<R>, key: &[u8], path: P) -> Result<()>
where
    R: Read + Seek,
    P: AsRef<Path>,
{
    let bytes = extract_attachment(pdf, key)?;
    std::fs::write(path, &bytes)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::super::{JobExitCode, QPDFJob};
    use super::extract_attachment;
    use super::AttachmentAddOptions;
    use super::PipelineHandleSink;
    use crate::job::attachment_list::list_attachment_info;
    use crate::pipeline::test_support::NthWriteFailure;
    use crate::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
    use crate::{Pdf, PdfOpenOptions, QPDFLogger};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct Capture {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    type CaptureBytes = Arc<Mutex<Vec<u8>>>;

    impl Pipeline for Capture {
        fn identifier(&self) -> &str {
            "attachment test capture"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.bytes
                .lock()
                .map_err(|_| PipelineError::runtime("capture mutex poisoned"))?
                .extend_from_slice(data);
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    #[test]
    fn capture_pipeline_exposes_its_identifier() {
        let capture = Capture {
            bytes: Arc::new(Mutex::new(Vec::new())),
        };
        assert_eq!(Pipeline::identifier(&capture), "attachment test capture");
        let sink = PipelineHandleSink(PipelineHandle::new(Capture {
            bytes: Arc::new(Mutex::new(Vec::new())),
        }));
        assert_eq!(Pipeline::identifier(&sink), "qpdf job attachment save");
    }

    fn job_with_captures() -> (QPDFJob, CaptureBytes, CaptureBytes) {
        let info = Arc::new(Mutex::new(Vec::new()));
        let save = Arc::new(Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_output_streams(
            Some(PipelineHandle::new(Capture {
                bytes: Arc::clone(&info),
            })),
            None,
        );
        logger
            .set_save(
                Some(PipelineHandle::new(Capture {
                    bytes: Arc::clone(&save),
                })),
                false,
            )
            .expect("capture save sink");
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        (job, info, save)
    }

    fn page_mode(pdf: &mut Pdf<Cursor<Vec<u8>>>) -> Option<Vec<u8>> {
        let root_ref = pdf.root_ref().expect("catalog root");
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root).expect("resolve catalog");
        root.get_key(b"/PageMode").as_name()
    }

    #[test]
    fn list_attachments_owns_no_embedded_files_message_and_completion() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ))
        .to_vec();
        let (mut job, info, _) = job_with_captures();
        let mut pdf = job
            .open(Cursor::new(bytes), "minimal.pdf", PdfOpenOptions::default())
            .expect("open fixture");

        let status = job
            .list_attachments(&mut pdf, false)
            .expect("list attachments");

        assert_eq!(status, JobExitCode::Success);
        assert_eq!(
            *info.lock().expect("info capture"),
            b"minimal.pdf has no embedded files\n"
        );
    }

    #[test]
    fn show_attachment_does_not_use_save_sink_when_key_is_missing() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let (mut job, _, save) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.as_slice()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let error = job
            .show_attachment(&mut pdf, b"missing")
            .expect_err("missing attachment must fail");

        assert!(error.to_string().contains("not found"));
        assert!(save.lock().expect("save capture").is_empty());
    }

    #[test]
    fn list_attachments_writes_qpdf_header_to_job_info_pipeline() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ));
        let (mut job, info, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "attachment-two-page.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        assert_eq!(
            job.list_attachments(&mut pdf, false)
                .expect("list attachments"),
            JobExitCode::Success
        );
        assert_eq!(
            *info.lock().expect("info capture"),
            b"attachment.txt -> 8,0\n"
        );
    }

    #[test]
    fn show_attachment_writes_decoded_bytes_to_job_save_pipeline() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ));
        let (mut job, _, save) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "attachment-two-page.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        assert_eq!(
            job.show_attachment(&mut pdf, b"attachment.txt")
                .expect("show attachment"),
            JobExitCode::Success
        );
        assert_eq!(
            *save.lock().expect("save capture"),
            b"This is a small text attachment for PDF fixture testing.\nGenerated by flpdf test corpus setup.\n"
        );
    }

    #[test]
    fn show_attachment_decode_failure_is_a_warning_for_an_existing_key() {
        let mut bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ))
        .to_vec();
        // Object 8's EF stream begins at this fixture offset. A broken zlib
        // header is a decode warning in qpdf's pipeStreamData route.
        bytes[1187] = 0;
        let (mut job, _, save) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes),
                "corrupt-attachment.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        assert_eq!(
            job.show_attachment(&mut pdf, b"attachment.txt")
                .expect("decode failure should complete with a warning"),
            JobExitCode::Warning
        );
        assert!(save.lock().expect("save capture").is_empty());
    }

    #[test]
    fn show_attachment_rejects_a_filespec_without_an_embedded_stream() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ));
        let (mut job, _, save) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "missing-embedded-stream.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");
        let filespec_ref = crate::ObjectRef::new(5, 0);
        let filespec = pdf.get_object_handle(filespec_ref);
        pdf.resolve(&filespec).expect("resolve filespec");
        filespec.remove_key(b"/EF");
        pdf.mark_object_handle_dirty(&filespec)
            .expect("mark Filespec dirty");

        let error = job
            .show_attachment(&mut pdf, b"attachment.txt")
            .expect_err("missing embedded stream must fail");
        assert!(error
            .to_string()
            .contains("no resolvable /EmbeddedFile stream"));
        assert!(save.lock().expect("save capture").is_empty());
    }

    #[test]
    fn add_attachment_owns_page_mode_metadata_name_tree_and_verbose_diagnostic() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let attachment = dir.path().join("payload.txt");
        std::fs::write(&attachment, b"attachment payload").expect("write payload");
        let (mut job, info, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        job.add_attachment(
            &mut pdf,
            AttachmentAddOptions {
                path: attachment.clone(),
                key: b"payload-key".to_vec(),
                filename: b"renamed.txt".to_vec(),
                mimetype: Some(b"text/plain".to_vec()),
                description: Some(b"test description".to_vec()),
                creation_date: Some(b"D:20240101120000Z".to_vec()),
                modification_date: Some(b"D:20240102130000Z".to_vec()),
                replace: false,
                verbose: true,
            },
        )
        .expect("add attachment");

        assert_eq!(page_mode(&mut pdf), Some(b"UseAttachments".to_vec()));

        let attachments = list_attachment_info(&mut pdf).expect("list attachments");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].key, b"payload-key");
        assert_eq!(attachments[0].display_name.as_deref(), Some("renamed.txt"));
        assert_eq!(
            attachments[0].mimetype.as_deref(),
            Some(b"text/plain".as_slice())
        );
        assert_eq!(
            attachments[0].description.as_deref(),
            Some(b"test description".as_slice())
        );
        assert_eq!(
            attachments[0].creation_date.as_deref(),
            Some(b"D:20240101120000Z".as_slice())
        );
        assert_eq!(
            attachments[0].modification_date.as_deref(),
            Some(b"D:20240102130000Z".as_slice())
        );

        let info = info.lock().expect("info capture");
        let info = String::from_utf8_lossy(&info);
        assert!(info.contains("qpdf: attached "));
        assert!(info.contains(" as renamed.txt with key payload-key\n"));
    }

    #[cfg(unix)]
    #[test]
    fn add_attachment_verbose_diagnostic_preserves_non_utf8_filename_and_key() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let attachment = dir.path().join("payload.txt");
        std::fs::write(&attachment, b"attachment payload").expect("write payload");
        let (mut job, info, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        // Invalid UTF-8: a lone continuation byte, which `from_utf8_lossy`
        // would replace with U+FFFD (b"\xEF\xBF\xBD").
        let non_utf8_filename = vec![b'f', 0x80, b'.', b't', b'x', b't'];
        let non_utf8_key = vec![b'k', 0x80];
        job.add_attachment(
            &mut pdf,
            AttachmentAddOptions {
                path: attachment,
                key: non_utf8_key.clone(),
                filename: non_utf8_filename.clone(),
                mimetype: None,
                description: None,
                creation_date: Some(b"D:20240101120000Z".to_vec()),
                modification_date: Some(b"D:20240101120000Z".to_vec()),
                replace: false,
                verbose: true,
            },
        )
        .expect("add attachment");

        let info = info.lock().expect("info capture");
        let mut expected = b" as ".to_vec();
        expected.extend_from_slice(&non_utf8_filename);
        expected.extend_from_slice(b" with key ");
        expected.extend_from_slice(&non_utf8_key);
        expected.push(b'\n');
        assert!(
            info.windows(expected.len())
                .any(|window| window == expected),
            "expected raw non-UTF-8 bytes in verbose diagnostic, got {info:?}"
        );
    }

    fn add_options(path: std::path::PathBuf, key: &[u8]) -> AttachmentAddOptions {
        AttachmentAddOptions {
            path,
            key: key.to_vec(),
            filename: b"payload.txt".to_vec(),
            mimetype: None,
            description: None,
            creation_date: Some(b"D:20240101120000Z".to_vec()),
            modification_date: Some(b"D:20240101120000Z".to_vec()),
            replace: false,
            verbose: false,
        }
    }

    #[test]
    fn add_attachment_defaults_dates_and_preserves_existing_page_mode() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let attachment = dir.path().join("payload.txt");
        std::fs::write(&attachment, b"payload").expect("write payload");
        let (mut job, _, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let root_ref = pdf.root_ref().expect("catalog root");
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root).expect("resolve catalog");
        root.replace_key(b"/PageMode", crate::ObjectHandle::name(b"UseNone".to_vec()))
            .expect("set existing page mode");
        pdf.mark_object_handle_dirty(&root)
            .expect("mark catalog dirty");

        let mut options = add_options(attachment, b"payload-key");
        options.creation_date = None;
        options.modification_date = None;
        job.add_attachment(&mut pdf, options)
            .expect("add attachment");

        assert_eq!(page_mode(&mut pdf), Some(b"UseNone".to_vec()));

        let attachments = list_attachment_info(&mut pdf).expect("list attachments");
        assert!(attachments[0]
            .creation_date
            .as_deref()
            .is_some_and(|date| date.starts_with(b"D:")));
        assert_eq!(
            attachments[0].creation_date, attachments[0].modification_date,
            "qpdf uses one current timestamp for both default dates"
        );
    }

    #[test]
    fn add_attachment_aggregates_duplicate_error_and_replaces_when_requested() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, b"first").expect("write first payload");
        std::fs::write(&second, b"second").expect("write second payload");
        let (mut job, _, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "input.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        job.add_attachment(&mut pdf, add_options(first, b"duplicate"))
            .expect("add first attachment");
        job.add_attachment(
            &mut pdf,
            add_options(dir.path().join("first.txt"), b"other"),
        )
        .expect("add second attachment");
        let error = job
            .add_attachments(
                &mut pdf,
                &[
                    add_options(second.clone(), b"duplicate"),
                    add_options(second.clone(), b"other"),
                ],
            )
            .expect_err("duplicate must fail without replace");
        assert_eq!(
            error.to_string(),
            "input.pdf already has attachments with the following keys: duplicate, other; use --replace to replace or --key to specify a different key"
        );

        let mut replacement = add_options(second, b"duplicate");
        replacement.replace = true;
        job.add_attachment(&mut pdf, replacement)
            .expect("replace attachment");
        assert_eq!(
            extract_attachment(&mut pdf, b"duplicate").expect("extract replacement"),
            b"second"
        );
    }

    #[test]
    fn add_attachment_accepts_raw_mimetype_at_the_library_boundary() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let attachment = dir.path().join("payload.txt");
        std::fs::write(&attachment, b"payload").expect("write payload");
        let (mut job, _, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");
        let mut options = add_options(attachment, b"payload-key");
        options.mimetype = Some(b"textplain".to_vec());

        job.add_attachment(&mut pdf, options)
            .expect("library boundary must not validate mimetype syntax");
        let attachments = list_attachment_info(&mut pdf).expect("list attachments");
        assert_eq!(
            attachments[0].mimetype.as_deref(),
            Some(b"textplain".as_slice())
        );
    }

    #[test]
    fn add_attachment_propagates_verbose_info_pipeline_failure() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let attachment = dir.path().join("payload.txt");
        std::fs::write(&attachment, b"payload").expect("write payload");

        let logger = QPDFLogger::create();
        logger.set_info(Some(PipelineHandle::new(NthWriteFailure::new(1))));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let mut options = add_options(attachment, b"payload-key");
        options.verbose = true;
        let error = job
            .add_attachment(&mut pdf, options)
            .expect_err("verbose info sink failure must propagate");
        assert_eq!(error.to_string(), "sink write failure 1");
    }

    fn rootless_fixture_bytes() -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let object_offset = bytes.len();
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{object_offset:010} 00000 n \ntrailer\n<< /Size 2 >>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        bytes
    }

    #[test]
    fn set_attachment_page_mode_rejects_a_pdf_without_catalog_root() {
        let mut pdf = Pdf::open(Cursor::new(rootless_fixture_bytes())).expect("open fixture");
        assert!(
            pdf.root_ref().is_none(),
            "fixture must have no catalog root"
        );
        let job = QPDFJob::new();
        let error = job
            .set_attachment_page_mode(&mut pdf)
            .expect_err("missing root must be rejected, matching qpdf's getRoot() throw");
        assert_eq!(error.to_string(), "missing required PDF entry: /Root");
    }

    #[test]
    fn add_attachment_rejects_a_pdf_without_catalog_root() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let attachment = dir.path().join("payload.txt");
        std::fs::write(&attachment, b"payload").expect("write payload");
        let (mut job, _, _) = job_with_captures();
        let mut pdf = Pdf::open(Cursor::new(rootless_fixture_bytes())).expect("open fixture");

        let error = job
            .add_attachment(&mut pdf, add_options(attachment, b"payload-key"))
            .expect_err("missing root must be rejected before creating any objects");
        assert_eq!(error.to_string(), "missing required PDF entry: /Root");
    }

    #[test]
    fn add_attachments_with_empty_batch_leaves_page_mode_untouched() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let (mut job, _, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");
        assert_eq!(
            page_mode(&mut pdf),
            None,
            "fixture must start without /PageMode"
        );

        job.add_attachments(&mut pdf, &[])
            .expect("empty batch must be a no-op");

        assert_eq!(
            page_mode(&mut pdf),
            None,
            "empty batch must not introduce /PageMode /UseAttachments"
        );
    }

    #[test]
    fn add_attachment_reports_missing_file_without_os_error_suffix() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let missing = dir.path().join("does-not-exist.bin");
        let (mut job, _, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let error = job
            .add_attachment(&mut pdf, add_options(missing.clone(), b"payload-key"))
            .expect_err("missing source file must fail");
        assert_eq!(
            error.to_string(),
            format!("open {}: No such file or directory", missing.display())
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_attachment_reports_permission_denied_without_os_error_suffix() {
        use std::os::unix::fs::PermissionsExt;

        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ));
        let dir = tempfile::tempdir().expect("temporary directory");
        let unreadable = dir.path().join("unreadable.bin");
        std::fs::write(&unreadable, b"payload").expect("write payload");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
            .expect("restrict permissions");
        if std::fs::File::open(&unreadable).is_ok() {
            // Root (or CAP_DAC_OVERRIDE) bypasses Unix permission bits
            // entirely, so this scenario cannot be exercised as such. CI
            // runs unprivileged, so this branch is never taken there.
            return; // cov:ignore: only reachable when the test runs as root
        }
        let (mut job, _, _) = job_with_captures();
        let mut pdf = job
            .open(
                Cursor::new(bytes.to_vec()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open fixture");

        let error = job
            .add_attachment(&mut pdf, add_options(unreadable.clone(), b"payload-key"))
            .expect_err("unreadable source file must fail");
        assert_eq!(
            error.to_string(),
            format!("open {}: Permission denied", unreadable.display())
        );
    }

    // ── copy_attachments ─────────────────────────────────────────────────────

    use super::AttachmentCopyOptions;

    fn minimal_fixture_bytes() -> Vec<u8> {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/minimal.pdf"
        ))
        .to_vec()
    }

    /// Build a fresh minimal-fixture `Pdf` (own throwaway `QPDFJob`, not the
    /// caller's) with `entries` embedded through [`QPDFJob::add_attachments`].
    fn pdf_with_attachments(
        dir: &std::path::Path,
        prefix: &str,
        entries: &[(&[u8], &[u8])],
    ) -> Pdf<Cursor<Vec<u8>>> {
        let mut job = QPDFJob::new();
        let mut pdf = job
            .open(
                Cursor::new(minimal_fixture_bytes()),
                "fixture.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open minimal fixture");
        let options: Vec<AttachmentAddOptions> = entries
            .iter()
            .enumerate()
            .map(|(i, (key, content))| {
                let path = dir.join(format!("{prefix}-{i}.bin"));
                std::fs::write(&path, content).expect("write payload");
                AttachmentAddOptions {
                    path,
                    key: key.to_vec(),
                    filename: key.to_vec(),
                    mimetype: None,
                    description: None,
                    creation_date: None,
                    modification_date: None,
                    replace: false,
                    verbose: false,
                }
            })
            .collect();
        job.add_attachments(&mut pdf, &options)
            .expect("build attachment fixture");
        pdf
    }

    fn copy_options(
        path: std::path::PathBuf,
        prefix: &[u8],
        verbose: bool,
    ) -> AttachmentCopyOptions {
        AttachmentCopyOptions {
            path,
            prefix: prefix.to_vec(),
            verbose,
        }
    }

    #[test]
    fn copy_attachments_copies_object_graph_with_prefix_and_verbose_diagnostics() {
        let source_bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        ));
        let mut source =
            Pdf::open(Cursor::new(source_bytes.to_vec())).expect("open source fixture");

        let (mut job, info, _) = job_with_captures();
        let mut target = job
            .open(
                Cursor::new(minimal_fixture_bytes()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open target fixture");

        job.copy_attachments(
            &mut target,
            &mut source,
            &copy_options(std::path::PathBuf::from("donor.pdf"), b"src-", true),
        )
        .expect("copy attachments");

        assert_eq!(
            page_mode(&mut target),
            Some(b"UseAttachments".to_vec()),
            "copyAttachments always sets /PageMode, even for a single entry"
        );

        let copied =
            extract_attachment(&mut target, b"src-attachment.txt").expect("extract copied file");
        assert_eq!(
            copied,
            b"This is a small text attachment for PDF fixture testing.\nGenerated by flpdf test corpus setup.\n"
        );

        let info = String::from_utf8_lossy(&info.lock().expect("info capture")).into_owned();
        assert!(
            info.contains("qpdf: copying attachments from donor.pdf\n"),
            "info was: {info:?}"
        );
        assert!(
            info.contains("  attachment.txt -> src-attachment.txt\n"),
            "info was: {info:?}"
        );
    }

    #[test]
    fn copy_attachments_aggregates_duplicate_keys_and_still_copies_the_rest() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let mut source = pdf_with_attachments(
            dir.path(),
            "src",
            &[
                (b"a".as_slice(), b"content a".as_slice()),
                (b"b".as_slice(), b"content b".as_slice()),
                (b"c".as_slice(), b"content c".as_slice()),
            ],
        );
        let mut target = pdf_with_attachments(
            dir.path(),
            "tgt",
            &[
                (b"a".as_slice(), b"existing a".as_slice()),
                (b"b".as_slice(), b"existing b".as_slice()),
            ],
        );

        let mut job = QPDFJob::new();
        job.set_input_name("target.pdf");
        let error = job
            .copy_attachments(
                &mut target,
                &mut source,
                &copy_options(std::path::PathBuf::from("donor.pdf"), b"", false),
            )
            .expect_err("colliding keys must be reported");

        assert_eq!(
            error.to_string(),
            "target.pdf already has attachments with keys that conflict with attachments from other files: file: donor.pdf, key: a; file: donor.pdf, key: b. Use --prefix with --copy-attachments-from or manually copy individual attachments."
        );

        // qpdf processes every entry before throwing: the non-colliding key
        // is still copied even though the batch as a whole reports an error.
        let copied = extract_attachment(&mut target, b"c").expect("non-colliding key copied");
        assert_eq!(copied, b"content c");
        assert!(target
            .embedded_files()
            .get_embedded_file(b"a")
            .expect("lookup")
            .is_some());
    }

    #[test]
    fn copy_attachments_returns_ok_for_an_empty_source_and_still_sets_page_mode() {
        let (mut job, _, _) = job_with_captures();
        let mut source = Pdf::open(Cursor::new(minimal_fixture_bytes())).expect("open donor");
        let mut target = job
            .open(
                Cursor::new(minimal_fixture_bytes()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open target fixture");

        job.copy_attachments(
            &mut target,
            &mut source,
            &copy_options(std::path::PathBuf::from("donor.pdf"), b"", false),
        )
        .expect("empty source copies nothing but still succeeds");

        assert_eq!(page_mode(&mut target), Some(b"UseAttachments".to_vec()));
        assert!(!job.has_warnings());
    }

    #[test]
    fn copy_attachments_installs_the_job_logger_on_source_as_well_as_target() {
        let (mut job, _, _) = job_with_captures();
        let mut source = Pdf::open(Cursor::new(minimal_fixture_bytes())).expect("open donor");
        let mut target = job
            .open(
                Cursor::new(minimal_fixture_bytes()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open target fixture");

        // Source starts with its own default logger, distinct from the job's.
        assert_ne!(source.logger(), job.logger());

        job.copy_attachments(
            &mut target,
            &mut source,
            &copy_options(std::path::PathBuf::from("donor.pdf"), b"", false),
        )
        .expect("empty source copy succeeds");

        assert_eq!(
            source.logger(),
            job.logger(),
            "a lazy warning raised while traversing source must reach the job's own sink, \
             not source's original (possibly silent) logger"
        );
    }

    #[test]
    fn copy_attachments_records_source_warnings_even_with_no_attachments() {
        // A `startxref` offset past EOF forces `repair: true` recovery,
        // which records a `Severity::Warning` repair diagnostic on the
        // opened document (qpdf: "file is damaged" / "Attempting to
        // reconstruct cross-reference table").
        let mut bytes = minimal_fixture_bytes();
        // The fixture's own trailer/startxref tail is replaced with one
        // pointing far beyond the file's length.
        let cut = bytes
            .windows(4)
            .rposition(|w| w == b"xref")
            .expect("fixture must contain an xref keyword");
        bytes.truncate(cut);
        bytes.extend_from_slice(
            b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 /Root 1 0 R >>\nstartxref\n999999\n%%EOF\n",
        );
        let source_options = PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        };
        let mut source = Pdf::open_with_options(Cursor::new(bytes), source_options)
            .expect("open damaged donor with recovery");
        assert!(
            !source.repair_diagnostics().entries().is_empty(),
            "fixture must actually trigger a repair diagnostic"
        );

        let (mut job, _, _) = job_with_captures();
        let mut target = job
            .open(
                Cursor::new(minimal_fixture_bytes()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open target fixture");

        job.copy_attachments(
            &mut target,
            &mut source,
            &copy_options(std::path::PathBuf::from("donor.pdf"), b"", false),
        )
        .expect("damaged-but-recovered source with no attachments still succeeds");

        assert!(
            job.has_warnings(),
            "source's repair diagnostics must fold into the job's own warning state"
        );
    }

    #[test]
    fn copy_attachments_encrypted_source_password_open_has_no_attachments() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v4-aes-128-r4.pdf"
        );
        let file = std::fs::File::open(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v4-aes-128-r4.pdf");
        let source_options = PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..PdfOpenOptions::default()
        };
        let mut source = Pdf::open_with_options(std::io::BufReader::new(file), source_options)
            .expect("open encrypted source");

        let (mut job, _, _) = job_with_captures();
        let mut target = job
            .open(
                Cursor::new(minimal_fixture_bytes()),
                "minimal.pdf",
                PdfOpenOptions::default(),
            )
            .expect("open target fixture");

        job.copy_attachments(
            &mut target,
            &mut source,
            &copy_options(std::path::PathBuf::from(path), b"", false),
        )
        .expect("encrypted fixture has no attachments; copy must succeed with zero entries");
    }
}
