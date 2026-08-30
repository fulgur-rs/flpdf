//! qpdf correspondence: QPDFFileSpecObjectHelper.cc.

use super::embedded_file_stream::EmbeddedFileStream;
use super::shared::{ensure_indirect_handle_belongs_to_pdf, format_pdf_date, NAME_KEYS};
use crate::object_handle::canonical_dictionary_key;
use crate::pdf_string::{new_unicode_string, utf8_value};
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::path::Path;

// ── FileSpec ──────────────────────────────────────────────────────────────────

/// Wrapper for a `/Filespec` dictionary (ISO 32000-1 §7.11.3).
///
/// Construct with [`FileSpec::new`], passing the [`ObjectHandle`] of a
/// `/Filespec` dictionary and a mutable borrow of the open document.
///
/// All accessors except [`embedded_file`](FileSpec::embedded_file) are
/// cheap dictionary lookups that return `Ok(None)` when the key is absent.
/// [`embedded_file`](FileSpec::embedded_file) resolves the `/EF /F` (or `/EF /UF`) indirect reference.
pub struct FileSpec<'a, R: Read + Seek + 'static> {
    /// qpdf's Filespec object handle. It may be direct or indirect.
    filespec: ObjectHandle,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FileSpec<'a, R> {
    /// Create an indirect `/Filespec` whose `/EF /F` and `/EF /UF` entries
    /// reference the same embedded-file stream, returning its object handle.
    ///
    /// This is the Rust form of qpdf's `createFileSpec` helper factory.
    pub fn create_file_spec(
        pdf: &mut Pdf<R>,
        filename: impl AsRef<[u8]>,
        embedded_file: ObjectHandle,
    ) -> Result<ObjectHandle> {
        let name = new_unicode_string(filename.as_ref());
        let embedded_file = if embedded_file.object_ref().is_some() {
            // qpdf's `QPDFObjectHandle::checkOwnership` compares the owning
            // QPDF of the value being inserted. It does not look up that
            // value by object number in the destination, since doing so would
            // register a foreign reference and alter subsequent allocation.
            if !pdf.is_canonical_object_handle(&embedded_file) {
                return Err(Error::Unsupported(
                    "embedded-file handle belongs to another Pdf".to_string(),
                ));
            }
            embedded_file
        } else {
            pdf.make_indirect_object_handle(embedded_file)?
        };
        let ef = ObjectHandle::dictionary(vec![
            (b"/F".to_vec(), embedded_file.clone()),
            (b"/UF".to_vec(), embedded_file),
        ]);
        let filespec = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Filespec".to_vec())),
            (b"/UF".to_vec(), ObjectHandle::string(name.clone())),
            (b"/F".to_vec(), ObjectHandle::string(name)),
            (b"/EF".to_vec(), ef),
        ]);
        pdf.make_indirect_object_handle(filespec)
    }

    /// Create a Filespec and embedded-file stream from a filesystem path.
    pub fn create_file_spec_from_path<P: AsRef<Path>>(
        pdf: &mut Pdf<R>,
        filename: impl AsRef<[u8]>,
        path: P,
    ) -> Result<ObjectHandle> {
        let embedded_file = EmbeddedFileStream::create_ef_stream_from_path(pdf, path)?;
        Self::create_file_spec(pdf, filename, embedded_file)
    }

    /// Construct a new wrapper for a `/Filespec` dictionary handle.
    ///
    /// The handle may be direct or indirect, matching qpdf's
    /// `QPDFFileSpecObjectHelper(QPDFObjectHandle)` constructor. An indirect
    /// handle must be the canonical handle of `pdf`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for an indirect handle owned by another
    /// [`Pdf`], preventing a same-number object in `pdf` from being selected.
    pub fn new(filespec: ObjectHandle, pdf: &'a mut Pdf<R>) -> Result<Self> {
        ensure_indirect_handle_belongs_to_pdf(&filespec, pdf, "Filespec")?;
        // QPDFFileSpecObjectHelper.cc:10-18 warns at construction time for a
        // non-dictionary value or a dictionary with the wrong /Type.
        let resolved = pdf.resolve_handle(&filespec)?;
        if resolved.try_as_dictionary()?.is_none() {
            resolved.warn_if_possible("Embedded file object is not a dictionary")?;
        } else if !resolved.try_is_dictionary_of_type(b"Filespec", b"")? {
            resolved.warn_if_possible("Embedded file object's type is not /Filespec")?;
        }
        Ok(Self { filespec, pdf })
    }

    /// Resolve the helper's object in place and retain its qpdf description.
    /// Accessors use this handle directly so `try_get_key` can emit the same
    /// `typeWarning` that qpdf's `QPDFObjectHandle::getKey` emits for a
    /// non-dictionary Filespec.
    fn filespec_handle(&mut self) -> Result<ObjectHandle> {
        let (filespec, terminal_ref) = self.pdf.resolve_handle_ref(&self.filespec)?;
        Ok(match terminal_ref {
            Some(object_ref) => {
                let filespec = self.pdf.get_object_handle(object_ref);
                self.pdf.resolve(&filespec)?;
                filespec
            }
            None => filespec,
        })
    }

    fn filespec_value(&mut self, key: &[u8]) -> Result<ObjectHandle> {
        let filespec = self.filespec_handle()?;
        if filespec.is_null() && filespec.context().is_none() {
            return Ok(ObjectHandle::null());
        }
        filespec.try_get_key(key)
    }

    fn resolved_string_value_handle(&mut self, value: ObjectHandle) -> Result<Option<Vec<u8>>> {
        let value = self.pdf.resolve_handle(&value)?;
        Ok(value.as_string())
    }

    fn filespec_dict(&mut self) -> Result<Option<ObjectHandle>> {
        let filespec = self.filespec_handle()?;
        Ok(filespec.as_dictionary().map(|_| filespec))
    }

    /// Set `/Desc` with qpdf's `newUnicodeString` storage semantics.
    pub fn set_description(&mut self, description: impl AsRef<[u8]>) -> Result<&mut Self> {
        let Some(dict) = self.filespec_dict()? else {
            return Ok(self);
        };
        self.pdf.mark_object_handle_dirty(&dict)?;
        dict.replace_key(
            b"/Desc",
            ObjectHandle::string(new_unicode_string(description.as_ref())),
        )?; // cov:ignore: FileSpec::new validates the receiver's document ownership
        Ok(self)
    }

    /// Set `/UF` and `/F` with qpdf's compatibility-filename behavior.
    ///
    /// `unicode_name` and a non-empty `compatibility_name` are byte sequences,
    /// matching qpdf's `std::string` parameters. The Unicode value is stored
    /// with `newUnicodeString`; the compatibility value is stored verbatim.
    pub fn set_filename(
        &mut self,
        unicode_name: impl AsRef<[u8]>,
        compatibility_name: Option<&[u8]>,
    ) -> Result<&mut Self> {
        let Some(dict) = self.filespec_dict()? else {
            return Ok(self);
        };
        self.pdf.mark_object_handle_dirty(&dict)?;
        let unicode_name = new_unicode_string(unicode_name.as_ref());
        dict.replace_key(b"/UF", ObjectHandle::string(unicode_name.clone()))?;
        let compatibility_name = compatibility_name
            .map(ToOwned::to_owned)
            .filter(|name| !name.is_empty());
        dict.replace_key(
            b"/F",
            ObjectHandle::string(compatibility_name.unwrap_or(unicode_name)),
        )?; // cov:ignore: FileSpec::new validates the receiver's document ownership
        Ok(self)
    }

    /// Return `/F` — the file name as raw PDF string bytes.
    ///
    /// Returns `None` when the key is absent or the value is not a string.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn filename(&mut self) -> Result<Option<Vec<u8>>> {
        let value = self.filespec_value(b"/F")?;
        self.resolved_string_value_handle(value)
    }

    /// Return `/UF` — the Unicode-encoded file name as raw PDF string bytes.
    ///
    /// `/UF` contains a UTF-16BE (with BOM) or PDFDocEncoding string.  The
    /// raw bytes are returned without decoding — callers may apply their own
    /// text-string decoder if needed.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn uf(&mut self) -> Result<Option<Vec<u8>>> {
        let value = self.filespec_value(b"/UF")?;
        self.resolved_string_value_handle(value)
    }

    /// Return `/Desc` — the file description as raw PDF string bytes.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn description(&mut self) -> Result<Option<Vec<u8>>> {
        let value = self.filespec_value(b"/Desc")?;
        self.resolved_string_value_handle(value)
    }

    /// Return `/AFRelationship` — the associated-file relationship as raw
    /// PDF name bytes (e.g. `b"Source"`, `b"Data"`, `b"Alternative"`).
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn af_relationship(&mut self) -> Result<Option<Vec<u8>>> {
        let value = self.filespec_value(b"/AFRelationship")?;
        Ok(self.pdf.resolve_handle(&value)?.as_name())
    }

    /// Return `/Desc` through qpdf's UTF-8 string view.
    ///
    /// The byte vector mirrors qpdf's `std::string`: an explicit UTF-8 BOM
    /// may be followed by invalid UTF-8, which Rust's [`String`] cannot hold.
    pub fn get_description(&mut self) -> Result<Vec<u8>> {
        let value = self.filespec_value(b"/Desc")?;
        Ok(self
            .resolved_string_value_handle(value)?
            .map(|value| utf8_value(&value))
            .unwrap_or_default())
    }

    /// Return the preferred file name using qpdf's `/UF`, `/F`, `/Unix`,
    /// `/DOS`, `/Mac` priority order and UTF-8 value conversion.
    pub fn get_filename(&mut self) -> Result<Vec<u8>> {
        for key in NAME_KEYS {
            let key = canonical_dictionary_key(key.as_bytes());
            let value = self.filespec_value(&key)?;
            if let Some(value) = self.resolved_string_value_handle(value)? {
                return Ok(utf8_value(&value));
            }
        }
        Ok(Vec::new())
    }

    /// Return every recognized Filespec name key whose value is a string.
    ///
    /// Keys retain qpdf's leading slash, e.g. `"/UF"`.
    pub fn get_filenames(&mut self) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut filenames = BTreeMap::new();
        for name_key in NAME_KEYS {
            let key = canonical_dictionary_key(name_key.as_bytes());
            let value = self.filespec_value(&key)?;
            if let Some(value) = self.resolved_string_value_handle(value)? {
                filenames.insert(format!("/{name_key}"), utf8_value(&value));
            }
        }
        Ok(filenames)
    }

    /// Return the raw `/EF` entry for `key`, or qpdf's null-object equivalent
    /// when `/EF` or the requested key is absent.
    ///
    /// An empty `key` performs qpdf's preferred stream lookup: it skips
    /// non-stream candidates and returns the first candidate that resolves to
    /// a stream, preserving the original reference when it was indirect.
    pub fn get_embedded_file_stream(&mut self, key: &str) -> Result<ObjectHandle> {
        let ef = self.get_embedded_file_streams()?;
        let ef = self.pdf.resolve_handle(&ef)?;
        let Some(entries) = ef.try_as_dictionary()? else {
            return Ok(ObjectHandle::null());
        };

        if !key.is_empty() {
            let key = canonical_dictionary_key(key.as_bytes());
            return ef.try_get_key(&key);
        }

        for key in NAME_KEYS {
            let key = canonical_dictionary_key(key.as_bytes());
            let Some(candidate) = entries.get(&key).cloned() else {
                continue;
            };
            let terminal = self.pdf.resolve_handle(&candidate)?;
            if terminal.as_stream_dict().is_some() {
                return Ok(candidate);
            }
        }
        Ok(ObjectHandle::null())
    }

    /// Return the raw `/EF` dictionary, or qpdf's null-object equivalent when
    /// the key is absent.
    pub fn get_embedded_file_streams(&mut self) -> Result<ObjectHandle> {
        self.filespec_value(b"/EF")
    }

    /// Return the raw `/EF` dictionary items in qpdf's `ditems()` order.
    ///
    /// `QPDF_Dictionary::ditems()` asks the receiver for its key set at both
    /// iterator construction and end comparison.  Retaining both calls is
    /// observable for a missing `/EF`: qpdf emits two `typeWarning`s while
    /// treating its null object as an empty dictionary.
    pub(crate) fn get_embedded_file_stream_entries(
        &mut self,
    ) -> Result<Vec<(Vec<u8>, ObjectHandle)>> {
        let ef = self.get_embedded_file_streams()?;
        if ef.is_null() && ef.context().is_none() {
            return Ok(Vec::new());
        }
        let keys = ef.try_get_keys()?;
        let _end_keys = ef.try_get_keys()?;
        keys.into_iter()
            .map(|key| Ok((key.clone(), ef.try_get_key(&key)?)))
            .collect()
    }

    /// Resolve and return the embedded file stream.
    ///
    /// The lookup priority for the `/EF` sub-dictionary key is
    /// `/UF`, `/F`, `/Unix`, `/DOS`, `/Mac` — the same preference order
    /// qpdf applies (Unicode name first), consistent with ISO 32000-1
    /// §7.11.4.  The first key that resolves to an `/EmbeddedFile` stream
    /// reference is used.
    ///
    /// Returns `Ok(None)` when the `/Filespec` dictionary has no `/EF` entry
    /// or when none of the standard keys (`/UF`, `/F`, `/Unix`, `/DOS`,
    /// `/Mac`) resolve to an `/EmbeddedFile` stream.
    ///
    /// A candidate key whose value is not an indirect reference, or that
    /// resolves to a non-stream object, is skipped and the search continues
    /// with the next key; if no key yields an `/EmbeddedFile` stream the
    /// method returns `Ok(None)` (it does not error on a non-stream entry).
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the `/Filespec` object itself is not a
    ///   dictionary.
    /// - Any error from [`Pdf::resolve`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use flpdf::{FileSpec, ObjectRef, Pdf};
    /// # use std::fs::File;
    /// # use std::io::BufReader;
    /// # let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// let mut fs = FileSpec::new(pdf.get_object_handle(ObjectRef::new(5, 0)), &mut pdf).unwrap();
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let bytes = ef.payload()?;
    ///     println!("{} bytes", bytes.len());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn embedded_file(&mut self) -> Result<Option<EmbeddedFileStream<'_, R>>> {
        let candidate = self.get_embedded_file_stream("")?;
        let (stream, terminal_ref) = self.pdf.resolve_handle_ref(&candidate)?;
        let Some(stream) = stream.as_stream_dict().map(|_| stream) else {
            return Ok(None);
        };
        let stream = terminal_ref
            .map(|object_ref| self.pdf.get_object_handle(object_ref))
            .unwrap_or(stream);
        Ok(Some(EmbeddedFileStream::new(stream, self.pdf)?))
    }
}

// ── FileSpecBuilder ───────────────────────────────────────────────────────────

/// Optional date fields for a `/Params` sub-dictionary.
///
/// Each tuple is `(year, month, day, hour, minute, second)`.
#[derive(Debug, Clone, Default)]
pub struct FileParamDates {
    /// `/Params /CreationDate` as `(year, month, day, hour, minute, second)`.
    pub creation: Option<(u16, u8, u8, u8, u8, u8)>,
    /// `/Params /ModDate` as `(year, month, day, hour, minute, second)`.
    pub modification: Option<(u16, u8, u8, u8, u8, u8)>,
}

/// Builder that constructs a `/Filespec` dictionary and its associated
/// `/EmbeddedFile` stream, then inserts both into a [`Pdf`] document.
///
/// Use [`FileSpecBuilder::new`] to create a builder, configure it with the
/// setter methods, then call [`FileSpecBuilder::build`] to write the objects
/// and obtain the filespec [`ObjectRef`].
///
/// # Example
///
/// ```no_run
/// # use flpdf::{filespec_helper::FileSpecBuilder, embedded_files, Pdf};
/// # use std::io::{BufReader, Cursor};
/// # let mut pdf: Pdf<Cursor<Vec<u8>>> = todo!();
/// let filespec_ref = FileSpecBuilder::new("report.pdf", b"...pdf bytes...")
///     .mimetype(b"application/pdf")
///     .description(b"Annual report")
///     .af_relationship(b"Data")
///     .build(&mut pdf)
///     .expect("build filespec");
/// embedded_files::insert_embedded_file(&mut pdf, b"report.pdf", filespec_ref)
///     .expect("insert into name tree");
/// ```
pub struct FileSpecBuilder {
    /// ASCII filename used for `/F`.
    filename: Vec<u8>,
    /// Unicode filename used for `/UF`; defaults to `filename` interpreted as UTF-8.
    uf_filename: Option<String>,
    /// Raw payload bytes for the `/EmbeddedFile` stream (uncompressed).
    payload: Vec<u8>,
    /// MIME type stored in `/EmbeddedFile /Subtype` (raw, e.g. `b"application/pdf"`).
    mimetype: Option<Vec<u8>>,
    /// Human-readable description stored in `/Filespec /Desc`.
    description: Option<Vec<u8>>,
    /// Associated-file relationship stored in `/Filespec /AFRelationship`.
    af_relationship: Option<Vec<u8>>,
    /// Optional date metadata for the `/Params` sub-dictionary.
    dates: FileParamDates,
}

impl FileSpecBuilder {
    /// Create a new builder for a file with the given ASCII `filename` and
    /// raw `payload` bytes.
    ///
    /// `filename` is stored verbatim in `/F`. `/UF` is encoded with qpdf's
    /// `newUnicodeString` behavior: it uses PDFDocEncoding when every
    /// character is representable and otherwise uses UTF-16BE with a BOM.
    /// For non-ASCII filenames, construct the builder with an ASCII fallback
    /// for `/F` and call [`Self::uf_filename`] with the original Unicode name.
    ///
    /// `payload` must be the **decoded** (uncompressed) bytes.  By default the
    /// builder writes them verbatim to the stream (no `/Filter`), matching
    /// qpdf's `QPDFEFStreamObjectHelper::createEFStream` factory.
    pub fn new(filename: impl AsRef<[u8]>, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            filename: filename.as_ref().to_vec(),
            uf_filename: None,
            payload: payload.into(),
            mimetype: None,
            description: None,
            af_relationship: None,
            dates: FileParamDates::default(),
        }
    }

    /// Set the Unicode filename stored in `/UF`, independently from `/F`.
    ///
    /// Use this when `/F` must be an ASCII-safe fallback but `/UF` should
    /// preserve the original Unicode filename.
    pub fn uf_filename(mut self, filename: impl AsRef<str>) -> Self {
        self.uf_filename = Some(filename.as_ref().to_string());
        self
    }

    /// Set the MIME type (stored in `/EmbeddedFile /Subtype`).
    ///
    /// `mime` should be the raw MIME type bytes, e.g. `b"application/pdf"`.
    /// The builder will escape `/` and other PDF delimiter bytes using `#XX`
    /// notation so that the name token is valid PDF syntax and round-trips
    /// correctly through the parser.
    pub fn mimetype(mut self, mime: impl AsRef<[u8]>) -> Self {
        self.mimetype = Some(mime.as_ref().to_vec());
        self
    }

    /// Set the file description (stored in `/Filespec /Desc`).
    pub fn description(mut self, desc: impl AsRef<[u8]>) -> Self {
        self.description = Some(desc.as_ref().to_vec());
        self
    }

    /// Set the `/AFRelationship` name (e.g. `b"Source"`, `b"Data"`).
    pub fn af_relationship(mut self, rel: impl AsRef<[u8]>) -> Self {
        self.af_relationship = Some(rel.as_ref().to_vec());
        self
    }

    /// Set the creation and/or modification dates for `/Params`.
    pub fn dates(mut self, dates: FileParamDates) -> Self {
        self.dates = dates;
        self
    }

    /// Build the `/Filespec` and `/EmbeddedFile` objects and insert them into
    /// `pdf`.  Returns the [`ObjectRef`] of the `/Filespec` dictionary.
    ///
    /// Two new indirect objects are allocated:
    /// - One `/EmbeddedFile` stream containing the payload.
    /// - One `/Filespec` dictionary pointing to the stream via `/EF`.
    ///
    /// The caller is responsible for inserting the returned ref into the
    /// document's `/Names /EmbeddedFiles` name tree.
    ///
    /// # Errors
    ///
    /// Returns an error only if object-number allocation fails (in practice
    /// this cannot happen with a well-formed document).
    pub fn build<R: Read + Seek>(self, pdf: &mut Pdf<R>) -> Result<ObjectRef> {
        let uf_filename = match self.uf_filename {
            Some(filename) => filename,
            None => std::str::from_utf8(&self.filename)
                .map_err(|_| {
                    Error::Unsupported(
                        "FileSpecBuilder: filename is not valid UTF-8; cannot encode /UF"
                            .to_string(),
                    )
                })?
                .to_string(),
        };

        // The two qpdf-shaped factories own all base dictionary construction:
        // `/Type`, `/Params /Size`, `/Params /CheckSum`, `/F`, `/UF`, and the
        // paired `/EF` references. The builder adds only its opt-in features.
        let stream_handle = EmbeddedFileStream::create_ef_stream(pdf, &self.payload)?;
        let filespec_handle = FileSpec::create_file_spec(pdf, &uf_filename, stream_handle)?;
        let filespec_ref = filespec_handle
            .object_ref()
            .expect("create_file_spec must create an indirect Filespec");
        {
            let mut filespec = FileSpec::new(pdf.get_object_handle(filespec_ref), pdf)?;
            filespec.set_filename(&uf_filename, Some(self.filename.as_slice()))?;
            if let Some(description) = self.description {
                filespec.set_description(description)?;
            }

            let mut embedded_file = filespec
                .embedded_file()?
                .expect("FileSpec::create must create an /EmbeddedFile stream");
            if let Some(mimetype) = self.mimetype {
                embedded_file.set_subtype(mimetype)?;
            }
            if let Some((y, mo, d, h, mi, s)) = self.dates.creation {
                embedded_file.set_creation_date(format_pdf_date(y, mo, d, h, mi, s))?;
            }
            if let Some((y, mo, d, h, mi, s)) = self.dates.modification {
                embedded_file.set_mod_date(format_pdf_date(y, mo, d, h, mi, s))?;
            }
        }

        if let Some(relationship) = self.af_relationship {
            pdf.mark_object_handle_dirty(&filespec_handle)?;
            filespec_handle.replace_key(b"/AFRelationship", ObjectHandle::name(relationship))?;
        }

        Ok(filespec_ref)
    }
}
