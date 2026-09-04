//! qpdf correspondence: QPDF_json.cc document input boundary for `createFromJSON`, `updateFromJSON`, and `importJSON` (`libqpdf/QPDF_json.cc:54-63,795-832`).
//!
//! The document boundary intentionally stays separate from [`super::input`]:
//! the reactor owns JSON state and object mutation, while this module owns the
//! qpdf rootless bootstrap, source lifetime, and exception/error boundary.

use super::input::JsonReactor;
use super::{parse_reader, JsonError};
use crate::{Error, Pdf, PdfOpenOptions, Result};
use std::cell::RefCell;
use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// qpdf's complete-create bootstrap (`libqpdf/QPDF_json.cc:54-63`).
///
/// This is deliberately not [`Pdf::empty`]. qpdf starts JSON creation with a
/// rootless document whose only trailer entry is `/Size 1`; the JSON trailer
/// then replaces that canonical trailer before import returns.
const JSON_PDF: &[u8] = concat!(
    "%PDF-1.3\n",
    "xref\n",
    "0 1\n",
    "0000000000 65535 f \n",
    "trailer << /Size 1 >>\n",
    "startxref\n",
    "9\n",
    "%%EOF\n",
)
.as_bytes();

/// Create a complete JSON-input document through the erased source boundary
/// used by [`crate::job::JobDocument`]. The rootless qpdf JSON seed remains
/// owned here so the job layer cannot accidentally substitute [`Pdf::empty`]
/// (`QPDF_json.cc:54-63`).
pub(crate) fn create_from_json_erased<S>(
    source: S,
    input_name: impl AsRef<[u8]>,
    mut options: PdfOpenOptions,
) -> Result<Pdf<Box<dyn crate::ReadSeek>>>
where
    S: Read + Seek + 'static,
{
    let input_name = input_name.as_ref().to_vec();
    options.description = input_name.clone();
    let mut pdf = Pdf::<Box<dyn crate::ReadSeek>>::open_with_options(
        Box::new(Cursor::new(JSON_PDF.to_vec())),
        options,
    )?; // cov:ignore: the fixed qpdf JSON rootless seed is a valid in-memory PDF
    pdf.import_json(source, input_name, true)?;
    Ok(pdf)
}

impl Pdf<Cursor<Vec<u8>>> {
    /// Create a PDF from a complete qpdf JSON v2 document.
    ///
    /// `source` is consumed incrementally and retained by any deferred stream
    /// providers created during import. `input_name` is the qpdf-style source
    /// description used for parser errors and semantic warning attribution.
    ///
    /// This mirrors `QPDF::createFromJSON(std::shared_ptr<InputSource>)` and
    /// therefore accepts arbitrarily large seekable JSON without first loading
    /// it into a second buffer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenFailure`] wrapping the terminal error when the
    /// JSON reactor recorded warnings before the import failed; the caller
    /// can drain those warnings through [`Error::open_failure`] the same way
    /// it drains a failed permissive PDF open.
    pub fn create_from_json<S>(source: S, input_name: impl AsRef<[u8]>) -> Result<Self>
    where
        S: Read + Seek + 'static,
    {
        Self::create_from_json_with_options(source, input_name, PdfOpenOptions::default())
    }

    /// Create a PDF from complete JSON with explicit qpdf document options.
    ///
    /// The options are applied to the rootless seed before the JSON reactor
    /// starts, so a caller-owned logger observes import-time warnings just as
    /// it observes warnings from later object resolution. This is the
    /// document half of `QPDFJob::createQPDF` (`QPDFJob.cc:429-462`,
    /// `QPDFJob.cc:1708`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::OpenFailure`] wrapping the terminal error when the
    /// JSON reactor recorded warnings before the import failed; the caller
    /// can drain those warnings through [`Error::open_failure`] the same way
    /// it drains a failed permissive PDF open.
    pub fn create_from_json_with_options<S>(
        source: S,
        input_name: impl AsRef<[u8]>,
        mut options: PdfOpenOptions,
    ) -> Result<Self>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.as_ref().to_vec();
        options.description = input_name.clone();
        let mut pdf = Self::open_mem_owned_with_options(JSON_PDF.to_vec(), options)?; // cov:ignore: the qpdf rootless seed is a fixed, valid in-memory PDF
        if let Err(error) = pdf.import_json(source, input_name, true) {
            let diagnostics = pdf.repair_diagnostics();
            return Err(Error::with_open_diagnostics(error, diagnostics));
        }
        Ok(pdf)
    }

    /// Create a PDF from a complete qpdf JSON v2 file.
    pub fn create_from_json_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let input_name = path_description_bytes(&path);
        let source = open_json_file(&path)?;
        Self::create_from_json(source, input_name)
    }
}

impl<R> Pdf<R>
where
    R: Read + Seek + 'static,
{
    /// Apply a partial qpdf JSON v2 document to this PDF.
    ///
    /// Objects omitted from the input remain unchanged. The input is consumed
    /// incrementally and may remain alive behind deferred stream providers.
    /// This mirrors `QPDF::updateFromJSON`.
    pub fn update_from_json<S>(&mut self, source: S, input_name: impl AsRef<[u8]>) -> Result<()>
    where
        S: Read + Seek + 'static,
    {
        self.import_json(source, input_name, false)
    }

    /// Apply a partial qpdf JSON v2 file to this PDF.
    pub fn update_from_json_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref().to_path_buf();
        let input_name = path_description_bytes(&path);
        let source = open_json_file(&path)?;
        self.update_from_json(source, input_name)
    }

    /// qpdf's private `importJSON` boundary shared by complete and update mode.
    pub(crate) fn import_json<S>(
        &mut self,
        source: S,
        input_name: impl AsRef<[u8]>,
        must_be_complete: bool,
    ) -> Result<()>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.as_ref().to_vec();
        // The tokenizer records offsets from its own first byte, not from
        // `source`'s absolute position (see `JsonReactor::with_stream_data_base_offset`),
        // so a caller-supplied reader that starts mid-stream (e.g. JSON
        // embedded after a prefix) needs this correction before any deferred
        // inline stream read seeks back into it. A failure here must not
        // silently substitute `0`: that would reintroduce the same corrupted
        // read for a `Seek` implementation whose relative `stream_position`
        // fails while absolute seeks still succeed.
        let mut source = source;
        let base_offset = source
            .stream_position()
            .map_err(|error| raw_system_error(&input_name, error.to_string().as_bytes()))?;
        let source = Rc::new(RefCell::new(source));
        let mut reactor = JsonReactor::new(
            self,
            Rc::clone(&source),
            input_name.clone(),
            must_be_complete,
        )
        .with_stream_data_base_offset(base_offset);

        // Drop the source borrow before inspecting the reactor. Providers keep
        // the Rc alive after this scope, but never run while registration is
        // taking place (`QPDF_json.cc:212-230`).
        let parsed = {
            let mut source = source.borrow_mut();
            parse_reader(&mut *source, Some(&mut reactor))
        };

        // qpdf's reactor throws immediately at the fatal condition
        // (`QPDF_json.cc:353,463`, etc.), unwinding out of `JSON::parse`
        // before the tokenizer can observe any later syntax error. flpdf's
        // reactor instead records the fatal and lets the tokenizer keep
        // running (subsequent reactor callbacks become no-ops, but parsing
        // itself does not stop) -- so a recorded fatal must be checked, and
        // reported, before any later-and-therefore-qpdf-unreachable parser
        // error `parsed` might carry from continuing past that point.
        if let Some(error) = reactor.fatal_error() {
            return Err(raw_system_error(&input_name, error.as_bytes()));
        }
        if let Err(error) = parsed {
            let detail: &[u8] = match &error {
                JsonError::Type(message) | JsonError::Parse(message) => message.as_bytes(),
            };
            return Err(raw_system_error(&input_name, detail));
        }
        if reactor.any_errors() {
            return Err(raw_system_error(&input_name, b"errors found in JSON"));
        }
        Ok(())
    }
}

fn raw_system_error(input_name: &[u8], detail: &[u8]) -> Error {
    let mut message = input_name.to_vec();
    message.extend_from_slice(b": ");
    message.extend_from_slice(detail);
    Error::SystemBytes(message)
}

fn open_json_file(path: &PathBuf) -> Result<File> {
    File::open(path).map_err(|error| Error::file_io("open JSON input", path.clone(), error))
}

fn path_description_bytes(path: &Path) -> Vec<u8> {
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
