//! qpdf correspondence: QPDF_json.cc document input boundary for `createFromJSON`, `updateFromJSON`, and `importJSON` (`libqpdf/QPDF_json.cc:54-63,795-832`).
//!
//! The document boundary intentionally stays separate from [`super::input`]:
//! the reactor owns JSON state and object mutation, while this module owns the
//! qpdf rootless bootstrap, source lifetime, and exception/error boundary.

use super::input::JsonReactor;
use super::parse_reader;
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
    pub fn create_from_json<S>(source: S, input_name: impl Into<String>) -> Result<Self>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        let mut pdf = Self::open_mem_owned_with_options(
            JSON_PDF.to_vec(),
            PdfOpenOptions {
                description: input_name.clone(),
                ..PdfOpenOptions::default()
            },
        )?; // cov:ignore: the qpdf rootless seed is a fixed, valid in-memory PDF
        pdf.import_json(source, input_name, true)?;
        Ok(pdf)
    }

    /// Create a PDF from a complete qpdf JSON v2 file.
    pub fn create_from_json_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let input_name = path.display().to_string();
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
    pub fn update_from_json<S>(&mut self, source: S, input_name: impl Into<String>) -> Result<()>
    where
        S: Read + Seek + 'static,
    {
        self.import_json(source, input_name, false)
    }

    /// Apply a partial qpdf JSON v2 file to this PDF.
    pub fn update_from_json_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref().to_path_buf();
        let input_name = path.display().to_string();
        let source = open_json_file(&path)?;
        self.update_from_json(source, input_name)
    }

    /// qpdf's private `importJSON` boundary shared by complete and update mode.
    pub(crate) fn import_json<S>(
        &mut self,
        source: S,
        input_name: impl Into<String>,
        must_be_complete: bool,
    ) -> Result<()>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        let source = Rc::new(RefCell::new(source));
        let mut reactor = JsonReactor::new(
            self,
            Rc::clone(&source),
            input_name.clone(),
            must_be_complete,
        );

        // Drop the source borrow before inspecting the reactor. Providers keep
        // the Rc alive after this scope, but never run while registration is
        // taking place (`QPDF_json.cc:212-230`).
        let parsed = {
            let mut source = source.borrow_mut();
            parse_reader(&mut *source, Some(&mut reactor))
        };

        if let Err(error) = parsed {
            return Err(Error::System(format!("{input_name}: {error}")));
        }
        if let Some(error) = reactor.fatal_error() {
            return Err(Error::System(format!("{input_name}: {error}")));
        }
        if reactor.any_errors() {
            return Err(Error::System(format!("{input_name}: errors found in JSON")));
        }
        Ok(())
    }
}

fn open_json_file(path: &PathBuf) -> Result<File> {
    File::open(path).map_err(|error| Error::file_io("open JSON input", path.clone(), error))
}
