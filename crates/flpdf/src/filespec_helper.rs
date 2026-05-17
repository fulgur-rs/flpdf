//! Typed wrappers for `/Filespec` dictionaries and `/EmbeddedFile` streams,
//! plus a builder for constructing them.
//!
//! [`FileSpec`] wraps a `/Filespec` dictionary and exposes ergonomic, typed
//! accessors for all common fields (filename, description, embedded file
//! stream, etc.).  [`EmbeddedFileStream`] wraps the embedded `/EmbeddedFile`
//! stream reachable via the `/EF` sub-dictionary and exposes its payload and
//! metadata (MIME type, dates, checksum, size).
//!
//! [`FileSpecBuilder`] constructs a `/Filespec` dictionary and its associated
//! `/EmbeddedFile` stream in-memory and writes them into a [`Pdf`] document via
//! [`Pdf::set_object`].  The returned [`ObjectRef`] can then be inserted into
//! the `/Names /EmbeddedFiles` name tree using
//! [`crate::embedded_files::insert_embedded_file`].
//!
//! Both reader types are **read-only**. [`FileSpec`] is a thin borrowing wrapper that
//! holds only the `/Filespec` `ObjectRef` and re-resolves the dictionary from
//! the live document on each accessor call. [`EmbeddedFileStream`] is
//! constructed once from an already-resolved `/EmbeddedFile` stream: it owns
//! that [`Stream`] and the `/Params` sub-dictionary resolved at construction
//! time (an indirect `/Params` is dereferenced once), so its metadata
//! accessors read this retained state rather than re-resolving.
//!
//! # Design
//!
//! PDF key naming follows ISO 32000-1 §7.11.  The `/EF` lookup priority used
//! here mirrors the qpdf JSON v2 `preferredcontents` order:
//! `/UF` › `/F` › `/Unix` › `/Mac` › `/DOS`.
//!
//! Date strings (e.g. `/Params /CreationDate`) are returned as raw PDF date
//! byte sequences (`D:YYYYMMDDHHmmSSOHH'mm'`).  No date parsing is performed.
//!
//! # Examples
//!
//! ## Read filename and payload from a `/Filespec` object
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::{BufReader, Cursor};
//! use flpdf::{FileSpec, ObjectRef, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
//!
//! // Assume we know the /Filespec object reference (e.g. from walking /Names).
//! let filespec_ref = ObjectRef::new(5, 0);
//! let mut fs = FileSpec::new(filespec_ref, &mut pdf);
//!
//! if let Some(name) = fs.filename()? {
//!     println!("filename: {}", String::from_utf8_lossy(&name));
//! }
//! if let Some(mut ef) = fs.embedded_file()? {
//!     let bytes = ef.payload()?;
//!     println!("{} payload bytes", bytes.len());
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Inspect embedded file metadata
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{FileSpec, ObjectRef, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
//! let filespec_ref = ObjectRef::new(5, 0);
//! let mut fs = FileSpec::new(filespec_ref, &mut pdf);
//!
//! if let Some(mut ef) = fs.embedded_file()? {
//!     if let Some(mime) = ef.mimetype()? {
//!         println!("MIME: {}", String::from_utf8_lossy(&mime));
//!     }
//!     if let Some(created) = ef.creation_date()? {
//!         // raw PDF date string, e.g. b"D:20260101000000Z"
//!         println!("created: {}", String::from_utf8_lossy(&created));
//!     }
//!     if let Some(sz) = ef.size()? {
//!         println!("uncompressed size: {sz}");
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::filters::{decode_stream_data, encode_stream_data};
use crate::object::{Dictionary, Object, Stream};
use crate::{Error, ObjectRef, Pdf, Result};
use md5::{Digest, Md5};
use std::io::{Read, Seek, Write};
use std::path::Path;

// ── EmbeddedFileStream ────────────────────────────────────────────────────────

/// Wrapper for a `/EmbeddedFile` stream (ISO 32000-1 §7.11.4).
///
/// Construct via [`FileSpec::embedded_file`] rather than directly.
///
/// All accessors are cheap: only [`payload`](EmbeddedFileStream::payload)
/// performs I/O (decoding the filter chain).
pub struct EmbeddedFileStream<'a, R: Read + Seek> {
    /// The resolved `/EmbeddedFile` stream.  Stored by value because `Stream`
    /// owns its data, and we need the dict reference to survive across calls.
    stream: Stream,
    /// The `/Params` sub-dictionary, resolved at construction time so that
    /// metadata accessors stay `&self`.  `/Params` may be given as an
    /// indirect reference, which is dereferenced here.
    params: Option<Dictionary>,
    /// Kept to hold the document borrow for the wrapper's lifetime,
    /// mirroring [`FileSpec`]'s exclusive-borrow semantics.
    #[allow(dead_code)]
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> EmbeddedFileStream<'a, R> {
    fn new(stream: Stream, pdf: &'a mut Pdf<R>) -> Result<Self> {
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => Some(d.clone()),
            Some(Object::Reference(r)) => {
                let r = *r;
                match pdf.resolve(r)? {
                    Object::Dictionary(d) => Some(d),
                    _ => None,
                }
            }
            _ => None,
        };
        Ok(Self {
            stream,
            params,
            pdf,
        })
    }

    /// Decode and return the raw payload bytes.
    ///
    /// Applies the stream's full filter chain (e.g. `/FlateDecode`) via
    /// [`crate::filters::decode_stream_data`].
    ///
    /// # Errors
    ///
    /// Propagates any error from the filter decoder (unsupported filter,
    /// corrupt data, etc.).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use flpdf::{FileSpec, ObjectRef, Pdf};
    /// # use std::fs::File;
    /// # use std::io::BufReader;
    /// # let mut pdf = Pdf::open(BufReader::new(File::open("a.pdf")?))?;
    /// # let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let data: Vec<u8> = ef.payload()?;
    ///     assert!(!data.is_empty());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn payload(&self) -> Result<Vec<u8>> {
        decode_stream_data(&self.stream.dict, &self.stream.data)
    }

    /// Return the MIME type from `/Subtype`, as raw bytes.
    ///
    /// `/Subtype` is a PDF name, e.g. `b"application/pdf"`.  Returns `None`
    /// when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases; never errors.
    pub fn mimetype(&self) -> Result<Option<Vec<u8>>> {
        Ok(match self.stream.dict.get("Subtype") {
            Some(Object::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Return the `/Params` sub-dictionary, if present.
    ///
    /// Resolved at construction time, so an indirect `/Params` reference is
    /// already dereferenced here.
    fn params(&self) -> Option<&Dictionary> {
        self.params.as_ref()
    }

    /// Return `/Params /CreationDate` as a raw PDF date byte sequence.
    ///
    /// PDF date format: `D:YYYYMMDDHHmmSSOHH'mm'` (ISO 32000-1 §7.9.4).
    /// No date parsing is performed — the bytes are returned as-is.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn creation_date(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.params().and_then(|p| match p.get("CreationDate") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        }))
    }

    /// Return `/Params /ModDate` as a raw PDF date byte sequence.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn modification_date(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.params().and_then(|p| match p.get("ModDate") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        }))
    }

    /// Return `/Params /CheckSum` as raw bytes (typically a 16-byte MD5 hash).
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn checksum(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.params().and_then(|p| match p.get("CheckSum") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        }))
    }

    /// Return `/Params /Size` — the uncompressed file size in bytes.
    ///
    /// # Errors
    ///
    /// Returns `Ok(None)` for all missing/wrong-type cases.
    pub fn size(&self) -> Result<Option<i64>> {
        Ok(self.params().and_then(|p| match p.get("Size") {
            Some(Object::Integer(n)) => Some(*n),
            _ => None,
        }))
    }
}

// ── FileSpec ──────────────────────────────────────────────────────────────────

/// Wrapper for a `/Filespec` dictionary (ISO 32000-1 §7.11.3).
///
/// Construct with [`FileSpec::new`], passing the [`ObjectRef`] of a
/// `/Filespec` dictionary and a mutable borrow of the open document.
///
/// All accessors except [`embedded_file`](FileSpec::embedded_file) are
/// cheap dictionary lookups that return `Ok(None)` when the key is absent.
/// [`embedded_file`] resolves the `/EF /F` (or `/EF /UF`) indirect reference.
pub struct FileSpec<'a, R: Read + Seek> {
    filespec_ref: ObjectRef,
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> FileSpec<'a, R> {
    /// Construct a new wrapper for the `/Filespec` dictionary at `filespec_ref`.
    ///
    /// The constructor does **not** resolve the reference — call individual
    /// accessors to fetch specific fields.
    pub fn new(filespec_ref: ObjectRef, pdf: &'a mut Pdf<R>) -> Self {
        Self { filespec_ref, pdf }
    }

    /// Resolve the `/Filespec` dictionary, returning an error when the
    /// object does not exist or is not a dictionary.
    fn resolve_dict(&mut self) -> Result<Dictionary> {
        match self.pdf.resolve(self.filespec_ref)? {
            Object::Dictionary(d) => Ok(d),
            _ => Err(Error::Unsupported(format!(
                "expected /Filespec dictionary at {}, got a non-dictionary object",
                self.filespec_ref
            ))),
        }
    }

    /// Return `/F` — the file name as raw PDF string bytes.
    ///
    /// Returns `None` when the key is absent or the value is not a string.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn filename(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("F") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
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
        let dict = self.resolve_dict()?;
        Ok(match dict.get("UF") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Return `/Desc` — the file description as raw PDF string bytes.
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn description(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("Desc") {
            Some(Object::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Return `/AFRelationship` — the associated-file relationship as raw
    /// PDF name bytes (e.g. `b"Source"`, `b"Data"`, `b"Alternative"`).
    ///
    /// # Errors
    ///
    /// Propagates any error from resolving the `/Filespec` object.
    pub fn af_relationship(&mut self) -> Result<Option<Vec<u8>>> {
        let dict = self.resolve_dict()?;
        Ok(match dict.get("AFRelationship") {
            Some(Object::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// Resolve and return the embedded file stream.
    ///
    /// The lookup priority for the `/EF` sub-dictionary key is
    /// `/UF`, `/F`, `/Unix`, `/Mac`, `/DOS` — the same preference order
    /// qpdf applies (Unicode name first), consistent with ISO 32000-1
    /// §7.11.4.  The first key that resolves to an `/EmbeddedFile` stream
    /// reference is used.
    ///
    /// Returns `Ok(None)` when the `/Filespec` dictionary has no `/EF` entry
    /// or when none of the standard keys (`/UF`, `/F`, `/Unix`, `/Mac`,
    /// `/DOS`) resolve to an `/EmbeddedFile` stream.
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
    /// let mut fs = FileSpec::new(ObjectRef::new(5, 0), &mut pdf);
    /// if let Some(mut ef) = fs.embedded_file()? {
    ///     let bytes = ef.payload()?;
    ///     println!("{} bytes", bytes.len());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn embedded_file(&mut self) -> Result<Option<EmbeddedFileStream<'_, R>>> {
        let dict = self.resolve_dict()?;

        // Resolve /EF sub-dictionary.
        let ef_dict: Dictionary = match dict.get("EF") {
            Some(Object::Dictionary(d)) => d.clone(),
            Some(Object::Reference(r)) => {
                let r = *r;
                match self.pdf.resolve(r)? {
                    Object::Dictionary(d) => d,
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        // qpdf preference order: Unicode name first, then platform-specific.
        // Try each key in order and skip any that does not resolve to an
        // /EmbeddedFile stream, so a stray non-stream entry on a
        // higher-priority key does not mask a valid lower-priority one.
        let candidates: Vec<ObjectRef> = ["UF", "F", "Unix", "Mac", "DOS"]
            .iter()
            .filter_map(|k| match ef_dict.get(k) {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            })
            .collect();

        for ef_ref in candidates {
            if let Object::Stream(stream) = self.pdf.resolve(ef_ref)? {
                return EmbeddedFileStream::new(stream, self.pdf).map(Some);
            }
        }

        Ok(None)
    }
}

// ── Encoding helpers ──────────────────────────────────────────────────────────

/// Encode a Unicode filename as a UTF-16BE string with BOM.
///
/// The returned bytes are: `[0xFE, 0xFF]` (BOM) followed by each UTF-16BE
/// code unit as two big-endian bytes.  This matches the `/UF` encoding
/// required by ISO 32000-1 §7.11.3.
///
/// # Examples
///
/// ```
/// use flpdf::filespec_helper::encode_utf16be;
///
/// let bytes = encode_utf16be("hi");
/// // BOM + 'h' (0x0068) + 'i' (0x0069)
/// assert_eq!(bytes, vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69]);
/// ```
pub fn encode_utf16be(s: &str) -> Vec<u8> {
    let mut out = vec![0xFE_u8, 0xFF]; // BOM
    for unit in s.encode_utf16() {
        out.push((unit >> 8) as u8);
        out.push((unit & 0xFF) as u8);
    }
    out
}

/// Format a date tuple `(year, month, day, hour, minute, second)` as a PDF
/// date string: `D:YYYYMMDDHHmmSSZ`.
///
/// The timezone suffix is always `Z` (UTC).  No validation of the individual
/// fields is performed.
///
/// # Examples
///
/// ```
/// use flpdf::filespec_helper::format_pdf_date;
///
/// assert_eq!(format_pdf_date(2026, 1, 1, 0, 0, 0), b"D:20260101000000Z".to_vec());
/// assert_eq!(format_pdf_date(2025, 12, 31, 23, 59, 59), b"D:20251231235959Z".to_vec());
/// ```
pub fn format_pdf_date(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Vec<u8> {
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
        year, month, day, hour, minute, second
    )
    .into_bytes()
}

// NOTE: a public `escape_pdf_name` helper used to live here. It was removed
// (roborev #920): `Object::Name` holds *decoded* logical bytes and the
// serializer escapes delimiters on write (#919), so escaping before
// constructing `Object::Name` would double-escape (`#` → `#23`). Callers must
// pass raw logical bytes straight to `Object::Name`. The canonical escaper now
// lives at `crate::object::escape_name_bytes` and is serializer-internal.

/// Compute the MD5 checksum of `data` and return it as a 16-byte `Vec<u8>`.
///
/// This is the checksum stored in `/Params /CheckSum` (ISO 32000-1 §7.11.4).
pub fn md5_checksum(data: &[u8]) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().to_vec()
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
    /// ASCII filename (used for `/F` and as the basis for `/UF`).
    filename: Vec<u8>,
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
    /// Whether to compress the stream payload with FlateDecode.
    ///
    /// When `true`, the `/EmbeddedFile` stream is compressed via
    /// `FlateDecode` using [`encode_stream_data`].  `/Params /Size` and
    /// `/Params /CheckSum` always reflect the **raw (uncompressed)** bytes
    /// regardless of this flag (ISO 32000-1 §7.11.4).
    compress: bool,
}

impl FileSpecBuilder {
    /// Create a new builder for a file with the given ASCII `filename` and
    /// raw `payload` bytes.
    ///
    /// `filename` is used for both `/F` (PDFDocEncoding) and `/UF` (UTF-16BE).
    /// For non-ASCII filenames, construct the builder with an ASCII fallback for
    /// `/F` and call no extra setter (UTF-16BE `/UF` is derived from `filename`
    /// via [`encode_utf16be`]).
    ///
    /// `payload` must be the **decoded** (uncompressed) bytes.  By default the
    /// builder writes them verbatim to the stream (no `/Filter`).  Call
    /// [`.compress(true)`](FileSpecBuilder::compress) to enable FlateDecode
    /// compression.
    pub fn new(filename: impl AsRef<[u8]>, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            filename: filename.as_ref().to_vec(),
            payload: payload.into(),
            mimetype: None,
            description: None,
            af_relationship: None,
            dates: FileParamDates::default(),
            compress: false,
        }
    }

    /// Enable or disable FlateDecode compression of the `/EmbeddedFile` stream
    /// payload (default: `false`).
    ///
    /// When `true`, the stream data is compressed via
    /// `crate::filters::encode_stream_data` with `/Filter /FlateDecode` before
    /// being stored.  `/Params /Size` and `/Params /CheckSum` always reflect the
    /// **raw (uncompressed)** bytes regardless of this setting.
    ///
    /// Compression is applied through `encode_stream_data`, which automatically
    /// inherits the `qpdf-zlib-compat` feature when enabled (byte-identical output
    /// to qpdf's `compress2()` at level 6).
    pub fn compress(mut self, compress: bool) -> Self {
        self.compress = compress;
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
        // Allocate two new object numbers.
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let stream_ref = ObjectRef::new(next + 1, 0);
        let filespec_ref = ObjectRef::new(next + 2, 0);

        // ── Build /Params sub-dictionary ─────────────────────────────────────
        let checksum = md5_checksum(&self.payload);
        let size = self.payload.len() as i64;

        let mut params = Dictionary::new();
        params.insert("Size", Object::Integer(size));
        params.insert("CheckSum", Object::String(checksum));
        if let Some((y, mo, d, h, mi, s)) = self.dates.creation {
            params.insert(
                "CreationDate",
                Object::String(format_pdf_date(y, mo, d, h, mi, s)),
            );
        }
        if let Some((y, mo, d, h, mi, s)) = self.dates.modification {
            params.insert(
                "ModDate",
                Object::String(format_pdf_date(y, mo, d, h, mi, s)),
            );
        }

        // ── Build /EmbeddedFile stream ────────────────────────────────────────
        let mut ef_dict = Dictionary::new();
        ef_dict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        if let Some(ref mime) = self.mimetype {
            // `Object::Name` holds raw (logical) bytes. The serializer escapes
            // delimiters as `#XX` on write and the parser decodes them on read,
            // so a MIME type like `application/pdf` round-trips correctly
            // through both in-memory access and write_pdf → reopen. Do NOT
            // pre-escape here — that would double-escape on serialization.
            ef_dict.insert("Subtype", Object::Name(mime.clone()));
        }

        // Compress if requested. /Params /Size and /Params /CheckSum are
        // always based on the raw (uncompressed) bytes — determined above —
        // regardless of compression.
        let (stream_payload, stored_length) = if self.compress {
            let mut enc_dict = Dictionary::new();
            enc_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            let encoded = encode_stream_data(&enc_dict, &self.payload)?;
            // Add /Filter to the EmbeddedFile stream dictionary so the decoder
            // knows how to decompress the payload.
            ef_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
            let encoded_len = encoded.len() as i64;
            (encoded, encoded_len)
        } else {
            (self.payload, size)
        };

        ef_dict.insert("Length", Object::Integer(stored_length));
        ef_dict.insert("Params", Object::Dictionary(params));

        let ef_stream = Stream::new(ef_dict, stream_payload);
        pdf.set_object(stream_ref, Object::Stream(ef_stream));

        // ── Build /EF sub-dictionary ──────────────────────────────────────────
        // Both /F and /UF point to the same stream ref (qpdf convention).
        let mut ef_sub = Dictionary::new();
        ef_sub.insert("F", Object::Reference(stream_ref));
        ef_sub.insert("UF", Object::Reference(stream_ref));

        // ── Build /Filespec dictionary ────────────────────────────────────────
        let uf_bytes = encode_utf16be(std::str::from_utf8(&self.filename).map_err(|_| {
            Error::Unsupported(
                "FileSpecBuilder: filename is not valid UTF-8; cannot encode /UF".to_string(),
            )
        })?);

        let mut fs_dict = Dictionary::new();
        fs_dict.insert("Type", Object::Name(b"Filespec".to_vec()));
        fs_dict.insert("F", Object::String(self.filename));
        fs_dict.insert("UF", Object::String(uf_bytes));
        fs_dict.insert("EF", Object::Dictionary(ef_sub));
        if let Some(desc) = self.description {
            fs_dict.insert("Desc", Object::String(desc));
        }
        if let Some(rel) = self.af_relationship {
            fs_dict.insert("AFRelationship", Object::Name(rel));
        }

        pdf.set_object(filespec_ref, Object::Dictionary(fs_dict));

        Ok(filespec_ref)
    }
}

// ── High-level attachment helper ──────────────────────────────────────────────

/// Load a file from disk and attach it to `pdf`, compressed with FlateDecode.
///
/// This is a convenience wrapper around [`FileSpecBuilder`] +
/// [`crate::embedded_files::insert_embedded_file`] that:
///
/// 1. Reads the file at `path` into memory.
/// 2. Derives the name-tree key and `/F`/`/UF` filename from the path's
///    **basename** (the last component of the path).
/// 3. Builds a `/Filespec` + `/EmbeddedFile` pair with FlateDecode compression.
///    `/Params /Size` and `/Params /CheckSum` reflect the **raw (uncompressed)**
///    bytes, as required by ISO 32000-1 §7.11.4.
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
/// - [`Error::Io`] if the file cannot be read.
/// - [`Error::Unsupported`] if the path has no basename, the basename is not
///   valid UTF-8, or the basename is not ASCII (independent `/F` ASCII-fallback
///   + `/UF` Unicode handling is not yet supported).
/// - Any error from [`FileSpecBuilder::build`] or
///   [`crate::embedded_files::insert_embedded_file`].
///
/// # Example
///
/// ```no_run
/// use std::io::Cursor;
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf: Pdf<Cursor<Vec<u8>>> = todo!();
/// let fs_ref = filespec_helper::add_attachment_from_path(
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

    // `FileSpecBuilder::new` uses the same string for both `/F`
    // (PDFDocEncoding/ASCII) and `/UF` (UTF-16BE).  A non-ASCII basename would
    // place non-PDFDocEncoding bytes into `/F`, corrupting the attachment name
    // in viewers that only read `/F`.  Reject it loudly until this helper can
    // set `/F` (ASCII-safe fallback) and `/UF` (full Unicode) independently
    // (tracked as a followup).
    if !basename.is_ascii() {
        return Err(Error::Unsupported(format!(
            "add_attachment_from_path: basename must be ASCII (independent /F \
             ASCII-fallback + /UF Unicode not yet supported): {}",
            path.display()
        )));
    }

    // Read the raw file bytes.
    let raw = std::fs::read(path)?;

    // Build the /Filespec + /EmbeddedFile and insert into the name tree.
    let filespec_ref = FileSpecBuilder::new(basename, raw)
        .compress(true)
        .build(pdf)?;
    crate::embedded_files::insert_embedded_file(pdf, key, filespec_ref)?;

    Ok(filespec_ref)
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
/// skipped with the same limitation documented there (see also the TODO comment
/// for flpdf-9hc.10.6). Only attachments with indirect-reference values are
/// extractable by this function.
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
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// let bytes = filespec_helper::extract_attachment(&mut pdf, b"report.pdf")?;
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
    let mut fs = FileSpec::new(filespec_ref, pdf);
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
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// let mut buf = Vec::new();
/// filespec_helper::write_attachment(&mut pdf, b"report.pdf", &mut buf)?;
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
/// use flpdf::{filespec_helper, Pdf};
///
/// # fn main() -> flpdf::Result<()> {
/// let mut pdf = Pdf::open(BufReader::new(File::open("with-attachment.pdf")?))?;
/// filespec_helper::extract_attachment_to_path(&mut pdf, b"report.pdf", "/tmp/out.pdf")?;
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

// ── Cross-document attachment copy ───────────────────────────────────────────

/// Recursively replace every `Object::Reference` inside `value` (and its
/// nested `Dictionary`/`Array` structure) with its **inlined** resolution
/// from `source`.
///
/// A copied `/EmbeddedFile` stream dictionary (or `/Filespec` dictionary) may
/// contain indirect references that only make sense in `source`'s object
/// table. Writing them verbatim into `target` would create dangling or
/// wrong-object references. Rather than deep-importing the whole subgraph
/// (and re-numbering objects), this inlines every resolvable reference to a
/// *value* and drops anything that cannot be inlined safely.
///
/// Returns `Ok(true)` if `value` is still valid in place, or `Ok(false)` if
/// the caller should drop the slot holding it (unresolvable reference,
/// non-inlineable target such as a nested `Stream`, cycle, or depth limit).
fn sanitize_imported_object<R: Read + Seek>(
    source: &mut Pdf<R>,
    value: &mut Object,
    visited: &mut std::collections::BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<bool> {
    if depth >= max_depth {
        return Ok(false);
    }
    match value {
        Object::Reference(r) => {
            let r = *r;
            if !visited.insert(r) {
                // Cycle / repeated reference — cannot safely inline again.
                return Ok(false);
            }
            let resolved = match source.resolve(r) {
                Ok(o) => o,
                Err(_) => {
                    visited.remove(&r);
                    return Ok(false);
                }
            };
            let keep = match resolved {
                // Non-inlineable: a stream or a chained reference.
                Object::Stream(_) | Object::Reference(_) => false,
                mut inner => {
                    let ok = sanitize_imported_object(
                        source,
                        &mut inner,
                        visited,
                        depth + 1,
                        max_depth,
                    )?;
                    if ok {
                        *value = inner;
                    }
                    ok
                }
            };
            visited.remove(&r);
            Ok(keep)
        }
        Object::Dictionary(dict) => {
            let keys: Vec<Vec<u8>> = dict.iter().map(|(k, _)| k.to_vec()).collect();
            for k in keys {
                let Some(slot) = dict.get(k.as_slice()).cloned() else {
                    continue;
                };
                let mut v = slot;
                if sanitize_imported_object(source, &mut v, visited, depth + 1, max_depth)? {
                    dict.insert(k.as_slice(), v);
                } else {
                    dict.remove(k.as_slice());
                }
            }
            Ok(true)
        }
        Object::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for elem in arr.iter() {
                let mut v = elem.clone();
                if sanitize_imported_object(source, &mut v, visited, depth + 1, max_depth)? {
                    out.push(v);
                }
            }
            *arr = out;
            Ok(true)
        }
        // Primitives are already self-contained.
        _ => Ok(true),
    }
}

// ── Cross-document attachment copy (continued) ───────────────────────────────

/// Copy all attachments from `source` into `target`, optionally prefixing each
/// name-tree key with `prefix`.
///
/// For each attachment in `source`'s `/Names /EmbeddedFiles` tree:
///
/// 1. The `/Filespec` dictionary is resolved (handles both indirect-reference
///    and direct-dictionary tree values).
/// 2. The `/EF` sub-dictionary is inspected with the qpdf key-priority order
///    `[UF, F, Unix, Mac, DOS]`; the first key whose value is an indirect
///    reference that resolves to a `/EmbeddedFile` stream is selected.
/// 3. The stream `data` bytes are copied verbatim (no decode/re-encode), and
///    the stream dictionary is **sanitized** ([`sanitize_imported_object`]):
///    every indirect reference it carries — at any nesting depth, including
///    `/Filter`, `/DecodeParms`, `/Params`, or refs nested inside an inlined
///    `/Params` — is inlined to its resolved value, and anything that cannot
///    be safely inlined (unresolvable, a nested stream, a cycle) is dropped,
///    so `target` never inherits a dangling or wrong-object foreign reference.
/// 4. A fresh `/Filespec` dictionary is built in `target`, copying `/Type`,
///    `/F`, `/UF`, `/Desc`, and `/AFRelationship` from the source filespec;
///    these source-derived fields are sanitized the same way. A new `/EF`
///    sub-dictionary is then created with `/F` and `/UF` both pointing to the
///    newly allocated (target) stream reference.
/// 5. The entry is inserted under the key `prefix + original_key` (or the
///    original key when `prefix` is `None`) via
///    [`crate::embedded_files::insert_embedded_file`], which replaces any
///    existing entry with the same key.
///
/// # Skip policy
///
/// Attachments whose `/EmbeddedFile` stream cannot be reached (e.g. the `/EF`
/// sub-dictionary is absent, none of the standard keys hold an indirect stream
/// reference, or the target object is not a stream) are **silently skipped**
/// and do not contribute to the returned count.  This keeps the operation
/// best-effort so a single malformed attachment in `source` does not abort the
/// entire copy.
///
/// # Return value
///
/// Returns the number of attachments successfully copied.  Returns `0` without
/// error when `source` has no attachments.
///
/// # Object numbering
///
/// Two new object numbers are allocated per copied attachment (one for the
/// `/EmbeddedFile` stream, one for the `/Filespec` dictionary).  The numbers
/// are derived from `max(target.object_refs().number) + 1`, snapshotted once
/// before the loop and incremented locally to avoid collisions across
/// attachments within the same call.
///
/// # Password-protected sources
///
/// `source` must already be opened (and authenticated) before being passed to
/// this function.  Use [`Pdf::open_with_options`] with a
/// [`PdfOpenOptions`](crate::PdfOpenOptions) carrying the password to open
/// an encrypted source document before calling `copy_attachments_from`.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`] (for both `source` and
/// `target`), from object allocation, or from
/// [`crate::embedded_files::insert_embedded_file`].  Individual attachment
/// failures that prevent stream resolution are skipped rather than propagated
/// (see the skip policy above).
pub fn copy_attachments_from<R1: Read + Seek, R2: Read + Seek>(
    target: &mut Pdf<R1>,
    source: &mut Pdf<R2>,
    prefix: Option<&[u8]>,
) -> Result<usize> {
    use crate::embedded_files::{
        collect_embedded_file_pairs_raw, insert_embedded_file, DEFAULT_MAX_EMBEDDED_FILES_DEPTH,
    };

    // Collect all (key, value) pairs from source — preserves both indirect
    // reference and direct-dict filespec values.
    let entries = collect_embedded_file_pairs_raw(source, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)?;
    if entries.is_empty() {
        return Ok(0);
    }

    let mut count = 0usize;

    for (key, value) in entries {
        // ── Step 1: resolve filespec dictionary ───────────────────────────────
        let fs_dict: Dictionary = match value {
            Object::Reference(r) => match source.resolve(r) {
                Ok(Object::Dictionary(d)) => d,
                _ => continue, // skip: cannot resolve filespec
            },
            Object::Dictionary(d) => d,
            _ => continue, // skip: unexpected value type
        };

        // ── Step 2: resolve /EF sub-dictionary ───────────────────────────────
        let ef_dict: Dictionary = match fs_dict.get("EF") {
            Some(Object::Dictionary(d)) => d.clone(),
            Some(Object::Reference(r)) => {
                let r = *r;
                match source.resolve(r) {
                    Ok(Object::Dictionary(d)) => d,
                    _ => continue, // skip: cannot resolve /EF
                }
            }
            _ => continue, // skip: no /EF
        };

        // ── Step 3 + 4: find the /EmbeddedFile stream in /EF and capture it ────
        // Use qpdf priority order: UF, F, Unix, Mac, DOS.
        // Capture the stream directly when found to avoid a second resolve call.
        let mut ef_stream: Stream = {
            let mut found = None;
            for k in &["UF", "F", "Unix", "Mac", "DOS"] {
                if let Some(Object::Reference(r)) = ef_dict.get(k) {
                    let r = *r;
                    if let Ok(Object::Stream(s)) = source.resolve(r) {
                        found = Some(s);
                        break;
                    }
                }
            }
            match found {
                Some(s) => s,
                None => continue, // skip: no resolvable /EmbeddedFile stream
            }
        };

        // Sanitize the copied stream dictionary: every indirect reference it
        // carries (e.g. `/Params`, `/DecodeParms`, `/Filter`, or anything
        // nested inside an inlined `/Params`) only makes sense in `source`'s
        // object table. Inline every resolvable reference to a value and drop
        // anything that cannot be safely inlined, so the target never inherits
        // a dangling or wrong-object foreign reference.
        {
            let mut dict_obj = Object::Dictionary(std::mem::take(&mut ef_stream.dict));
            let mut visited = std::collections::BTreeSet::new();
            sanitize_imported_object(
                source,
                &mut dict_obj,
                &mut visited,
                0,
                DEFAULT_MAX_EMBEDDED_FILES_DEPTH,
            )?;
            if let Object::Dictionary(d) = dict_obj {
                ef_stream.dict = d;
            }
        }

        // Allocate two fresh object numbers in target for this attachment.
        // Re-snapshot the max after every `insert_embedded_file` call so that
        // object numbers allocated by the name-tree rebuild inside that call are
        // accounted for and do not collide with our next pair.
        let base: u32 = target
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let new_stream_ref = ObjectRef::new(base + 1, 0);
        let new_fs_ref = ObjectRef::new(base + 2, 0);

        // Write the copied stream into target (bytes + dict, verbatim).
        target.set_object(new_stream_ref, Object::Stream(ef_stream));

        // ── Step 5: build a new /Filespec dictionary in target ─────────────────
        let mut new_fs = Dictionary::new();
        // Copy /Type from source filespec (usually "Filespec").
        if let Some(t) = fs_dict.get("Type") {
            new_fs.insert("Type", t.clone());
        }
        // Copy filename fields.
        if let Some(f) = fs_dict.get("F") {
            new_fs.insert("F", f.clone());
        }
        if let Some(uf) = fs_dict.get("UF") {
            new_fs.insert("UF", uf.clone());
        }
        // Copy optional metadata.
        if let Some(desc) = fs_dict.get("Desc") {
            new_fs.insert("Desc", desc.clone());
        }
        if let Some(rel) = fs_dict.get("AFRelationship") {
            new_fs.insert("AFRelationship", rel.clone());
        }

        // Sanitize the source-derived filespec fields BEFORE attaching the
        // target `/EF` reference: these values were cloned from `source`'s
        // filespec and may carry foreign indirect references. The `/EF`
        // sub-dict is inserted afterwards so the (valid) target stream ref is
        // never run through `source.resolve`.
        {
            let mut fs_obj = Object::Dictionary(std::mem::take(&mut new_fs));
            let mut visited = std::collections::BTreeSet::new();
            sanitize_imported_object(
                source,
                &mut fs_obj,
                &mut visited,
                0,
                DEFAULT_MAX_EMBEDDED_FILES_DEPTH,
            )?;
            if let Object::Dictionary(d) = fs_obj {
                new_fs = d;
            }
        }

        // /EF sub-dict: both /F and /UF point to the new (target) stream ref.
        let mut new_ef_sub = Dictionary::new();
        new_ef_sub.insert("F", Object::Reference(new_stream_ref));
        new_ef_sub.insert("UF", Object::Reference(new_stream_ref));
        new_fs.insert("EF", Object::Dictionary(new_ef_sub));

        target.set_object(new_fs_ref, Object::Dictionary(new_fs));

        // ── Step 6: build the name-tree key (with optional prefix) ──────────
        let new_key: Vec<u8> = match prefix {
            Some(p) => p.iter().chain(key.iter()).copied().collect(),
            None => key,
        };

        // Insert into target's name tree.
        insert_embedded_file(target, &new_key, new_fs_ref)?;

        count += 1;
    }

    Ok(count)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_files::{insert_embedded_file, list_embedded_files};
    use crate::filters::decode_stream_data;
    use crate::{Object, ObjectRef, Pdf};
    use std::io::Cursor;

    // ── Minimal PDF fixture ───────────────────────────────────────────────────

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_minimal() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF")
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Resolve the /EmbeddedFile stream dict for a filespec ref.
    fn resolve_ef_stream(
        pdf: &mut Pdf<Cursor<Vec<u8>>>,
        fs_ref: ObjectRef,
    ) -> crate::object::Stream {
        let fs_obj = pdf.resolve(fs_ref).expect("resolve filespec");
        let Object::Dictionary(fs_dict) = fs_obj else {
            panic!("expected dictionary");
        };
        let ef_sub = match fs_dict.get("EF") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /EF"),
        };
        let stream_ref = match ef_sub.get("F") {
            Some(Object::Reference(r)) => *r,
            _ => panic!("missing /EF /F ref"),
        };
        match pdf.resolve(stream_ref).expect("resolve stream") {
            Object::Stream(s) => s,
            _ => panic!("expected stream"),
        }
    }

    // ── Tests: FileSpecBuilder with compress(false) — existing behaviour ───────

    #[test]
    fn builder_uncompressed_round_trip() {
        let mut pdf = open_minimal();
        let raw = b"hello world";
        let fs_ref = FileSpecBuilder::new("test.txt", raw.as_ref())
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        // No /Filter in uncompressed stream
        assert!(
            stream.dict.get("Filter").is_none(),
            "uncompressed stream must have no /Filter"
        );
        let decoded = decode_stream_data(&stream.dict, &stream.data).expect("decode");
        assert_eq!(decoded, raw);
    }

    // ── Tests: FileSpecBuilder with compress(true) ────────────────────────────

    #[test]
    fn builder_compressed_has_flatedecode_filter() {
        let mut pdf = open_minimal();
        let raw = b"compressed payload data";
        let fs_ref = FileSpecBuilder::new("data.bin", raw.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Name(b"FlateDecode".to_vec())),
            "/Filter must be /FlateDecode"
        );
    }

    #[test]
    fn builder_compressed_round_trip() {
        let mut pdf = open_minimal();
        let raw = b"The quick brown fox jumps over the lazy dog.";
        let fs_ref = FileSpecBuilder::new("fox.txt", raw.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        let decoded = decode_stream_data(&stream.dict, &stream.data).expect("decode");
        assert_eq!(
            decoded.as_slice(),
            raw.as_ref(),
            "round-trip must restore original bytes"
        );
    }

    #[test]
    fn builder_compressed_params_size_is_raw_length() {
        let mut pdf = open_minimal();
        let raw = b"some raw bytes for size check";
        let fs_ref = FileSpecBuilder::new("size.bin", raw.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /Params"),
        };
        let stored_size = match params.get("Size") {
            Some(Object::Integer(n)) => *n,
            _ => panic!("missing /Params /Size"),
        };
        assert_eq!(
            stored_size,
            raw.len() as i64,
            "/Params /Size must equal raw byte length, not compressed length"
        );
        // Compressed payload should differ from raw (sanity check)
        assert_ne!(
            stream.data.len(),
            raw.len(),
            "compressed data length should differ from raw (sanity)"
        );
    }

    #[test]
    fn builder_compressed_params_checksum_is_md5_of_raw() {
        let mut pdf = open_minimal();
        let raw = b"checksum test data 12345";
        let fs_ref = FileSpecBuilder::new("chk.bin", raw.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /Params"),
        };
        let stored_checksum = match params.get("CheckSum") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /Params /CheckSum"),
        };
        let expected = md5_checksum(raw);
        assert_eq!(
            stored_checksum, expected,
            "/Params /CheckSum must be MD5 of raw bytes"
        );
    }

    #[test]
    fn builder_compressed_f_and_uf_are_basename() {
        let mut pdf = open_minimal();
        let raw = b"payload";
        let fs_ref = FileSpecBuilder::new("myfile.txt", raw.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build");

        let fs_obj = pdf.resolve(fs_ref).expect("resolve filespec");
        let Object::Dictionary(fs_dict) = fs_obj else {
            panic!("expected dictionary");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };
        assert_eq!(f, b"myfile.txt", "/F must be the filename");
        let expected_uf = encode_utf16be("myfile.txt");
        assert_eq!(uf, expected_uf, "/UF must be UTF-16BE encoded filename");
    }

    // ── Tests: FileSpecBuilder → insert_embedded_file → list ─────────────────

    #[test]
    fn compressed_filespec_retrievable_via_list() {
        let mut pdf = open_minimal();
        let raw = b"retrievable payload";
        let fs_ref = FileSpecBuilder::new("list-test.txt", raw.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"list-test.txt", fs_ref).expect("insert");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"list-test.txt");
        assert_eq!(entries[0].1, fs_ref);
    }

    #[test]
    fn existing_attachment_survives_second_insertion() {
        let mut pdf = open_minimal();

        // Insert first attachment (uncompressed for variety)
        let raw1 = b"first attachment";
        let fs1 = FileSpecBuilder::new("first.txt", raw1.as_ref())
            .build(&mut pdf)
            .expect("build first");
        insert_embedded_file(&mut pdf, b"first.txt", fs1).expect("insert first");

        // Insert second attachment (compressed)
        let raw2 = b"second attachment with more data";
        let fs2 = FileSpecBuilder::new("second.txt", raw2.as_ref())
            .compress(true)
            .build(&mut pdf)
            .expect("build second");
        insert_embedded_file(&mut pdf, b"second.txt", fs2).expect("insert second");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 2, "both attachments must survive");
        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(
            keys.contains(&b"first.txt".as_ref()),
            "first.txt must be present"
        );
        assert!(
            keys.contains(&b"second.txt".as_ref()),
            "second.txt must be present"
        );
    }

    // ── Tests: add_attachment_from_path ───────────────────────────────────────

    #[test]
    fn add_attachment_from_path_round_trip() {
        let mut pdf = open_minimal();

        // Write a temp file to attach.
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("hello.txt");
        let raw = b"Hello from disk!";
        std::fs::write(&file_path, raw).expect("write temp file");

        let fs_ref = add_attachment_from_path(&mut pdf, b"hello.txt", &file_path).expect("attach");

        // Verify retrievable via list_embedded_files
        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, b"hello.txt");
        assert_eq!(entries[0].1, fs_ref);

        // Verify round-trip decompression
        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Name(b"FlateDecode".to_vec())),
            "must use FlateDecode"
        );
        let decoded = decode_stream_data(&stream.dict, &stream.data).expect("decode");
        assert_eq!(
            decoded.as_slice(),
            raw.as_ref(),
            "round-trip must restore original bytes"
        );
    }

    #[test]
    fn add_attachment_from_path_checksum_and_size() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("data.bin");
        let raw = b"deterministic checksum test data";
        std::fs::write(&file_path, raw).expect("write");

        let fs_ref = add_attachment_from_path(&mut pdf, b"data.bin", &file_path).expect("attach");

        let stream = resolve_ef_stream(&mut pdf, fs_ref);
        let params = match stream.dict.get("Params") {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => panic!("missing /Params"),
        };
        let size = match params.get("Size") {
            Some(Object::Integer(n)) => *n,
            _ => panic!("missing /Params /Size"),
        };
        let checksum = match params.get("CheckSum") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /Params /CheckSum"),
        };
        assert_eq!(
            size,
            raw.len() as i64,
            "/Params /Size must match raw length"
        );
        assert_eq!(
            checksum,
            md5_checksum(raw),
            "/Params /CheckSum must match MD5 of raw bytes"
        );
    }

    #[test]
    fn add_attachment_from_path_f_and_uf_are_basename() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("report.pdf");
        std::fs::write(&file_path, b"fake pdf content").expect("write");

        let fs_ref = add_attachment_from_path(&mut pdf, b"report.pdf", &file_path).expect("attach");

        let fs_obj = pdf.resolve(fs_ref).expect("resolve");
        let Object::Dictionary(fs_dict) = fs_obj else {
            panic!("expected dict");
        };
        let f = match fs_dict.get("F") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /F"),
        };
        let uf = match fs_dict.get("UF") {
            Some(Object::String(b)) => b.clone(),
            _ => panic!("missing /UF"),
        };
        assert_eq!(f, b"report.pdf", "/F must be basename");
        assert_eq!(
            uf,
            encode_utf16be("report.pdf"),
            "/UF must be UTF-16BE basename"
        );
    }

    #[test]
    fn add_attachment_from_path_errors_on_missing_file() {
        let mut pdf = open_minimal();
        let result =
            add_attachment_from_path(&mut pdf, b"missing.txt", "/this/does/not/exist/missing.txt");
        assert!(result.is_err(), "must error when file does not exist");
        // Should be an Io error
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::Error::Io(_)),
            "expected Io error, got: {err:?}"
        );
    }

    #[test]
    fn add_attachment_from_path_rejects_non_ascii_basename() {
        // A non-ASCII basename would put non-PDFDocEncoding bytes into `/F`,
        // corrupting the attachment name in viewers that ignore `/UF`.  The
        // helper must reject it loudly rather than silently corrupt `/F`.
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("é.txt");
        std::fs::write(&file_path, b"payload").expect("write temp file");

        let result = add_attachment_from_path(&mut pdf, b"e.txt", &file_path);
        assert!(result.is_err(), "must reject non-ASCII basename");
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(_)),
            "expected Unsupported error, got: {err:?}"
        );
    }

    // ── Tests: extract_attachment / write_attachment / extract_attachment_to_path ─

    #[test]
    fn extract_attachment_small_round_trip() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("small.txt");
        let raw = b"Hello, world!";
        std::fs::write(&path, raw).expect("write");
        add_attachment_from_path(&mut pdf, b"small.txt", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"small.txt").expect("extract");
        assert_eq!(
            extracted.as_slice(),
            raw.as_ref(),
            "small file round-trip must match"
        );
    }

    #[test]
    fn extract_attachment_large_round_trip() {
        // 128 KiB of repeating pseudo-random-ish bytes — exercises compressor splits.
        let raw: Vec<u8> = (0u8..=255).cycle().take(128 * 1024).collect();
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.bin");
        std::fs::write(&path, &raw).expect("write");
        add_attachment_from_path(&mut pdf, b"large.bin", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"large.bin").expect("extract");
        assert_eq!(extracted, raw, "large file round-trip must match");
    }

    #[test]
    fn extract_attachment_binary_with_nuls_round_trip() {
        // 4096 bytes including NUL bytes, exercises binary safety.
        let raw: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, &raw).expect("write");
        add_attachment_from_path(&mut pdf, b"binary.bin", &path).expect("attach");

        let extracted = extract_attachment(&mut pdf, b"binary.bin").expect("extract");
        assert_eq!(extracted, raw, "binary file round-trip must match");
    }

    #[test]
    fn write_attachment_to_vec_matches_extract() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vec-test.txt");
        let raw = b"write_attachment test payload";
        std::fs::write(&path, raw).expect("write");
        add_attachment_from_path(&mut pdf, b"vec-test.txt", &path).expect("attach");

        let mut buf = Vec::new();
        write_attachment(&mut pdf, b"vec-test.txt", &mut buf).expect("write_attachment");
        assert_eq!(
            buf.as_slice(),
            raw.as_ref(),
            "write_attachment output must match raw"
        );
    }

    #[test]
    fn extract_attachment_to_path_round_trip() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");

        let src_path = dir.path().join("source.bin");
        let raw: Vec<u8> = (0u8..=255).cycle().take(2048).collect();
        std::fs::write(&src_path, &raw).expect("write source");
        add_attachment_from_path(&mut pdf, b"source.bin", &src_path).expect("attach");

        let out_path = dir.path().join("extracted.bin");
        extract_attachment_to_path(&mut pdf, b"source.bin", &out_path)
            .expect("extract_attachment_to_path");

        let read_back = std::fs::read(&out_path).expect("read back");
        assert_eq!(read_back, raw, "extract_to_path round-trip must match");
    }

    #[test]
    fn extract_attachment_missing_key_is_actionable_error() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("real.txt");
        std::fs::write(&path, b"real content").expect("write");
        add_attachment_from_path(&mut pdf, b"real.txt", &path).expect("attach");

        let err =
            extract_attachment(&mut pdf, b"missing-key").expect_err("must error for absent key");
        let msg = err.to_string();
        assert!(
            msg.contains("missing-key"),
            "error message must contain the missing key name, got: {msg}"
        );
        // Available keys hint must be present
        assert!(
            msg.contains("real.txt"),
            "error message must list available keys, got: {msg}"
        );
    }

    #[test]
    fn extract_attachment_from_compat_fixture() {
        // attachment-two-page.pdf contains an attachment under the key "attachment.txt"
        // with an uncompressed size of 95 bytes (from /Params /Size).
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/compat/attachment-two-page.pdf"
        );
        // The compat fixture is committed to the repo, so a missing file is a
        // real regression — fail loudly instead of silently skipping, which
        // could turn this into a false-positive pass (CodeRabbit).
        let file = std::fs::File::open(path)
            .expect("compat fixture missing: tests/fixtures/compat/attachment-two-page.pdf");
        let mut pdf = crate::Pdf::open(std::io::BufReader::new(file)).expect("open compat fixture");

        let entries = crate::embedded_files::list_embedded_files(&mut pdf).expect("list");
        assert!(
            !entries.is_empty(),
            "fixture must have at least one attachment"
        );

        // Use the first available key.
        let key = entries[0].0.clone();
        let extracted = extract_attachment(&mut pdf, &key).expect("extract from compat fixture");
        assert!(!extracted.is_empty(), "extracted bytes must be non-empty");

        // The fixture reports /Params /Size 95 — the extracted bytes must match.
        let mut fs = FileSpec::new(entries[0].1, &mut pdf);
        let ef = fs
            .embedded_file()
            .expect("embedded_file")
            .expect("must have embedded file");
        let reported_size = ef.size().expect("size").expect("size must be present");
        assert_eq!(
            extracted.len() as i64,
            reported_size,
            "extracted length must equal /Params /Size"
        );
    }

    // ── Tests: copy_attachments_from ──────────────────────────────────────────

    /// Build a two-attachment source PDF and copy all into an empty target;
    /// verify both payloads round-trip via extract_attachment.
    #[test]
    fn copy_attachments_two_files_round_trip() {
        // Build source PDF with two attachments.
        let mut source = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path_a = dir.path().join("alpha.txt");
        let path_b = dir.path().join("beta.txt");
        let raw_a = b"attachment alpha payload";
        let raw_b = b"attachment beta payload";
        std::fs::write(&path_a, raw_a).unwrap();
        std::fs::write(&path_b, raw_b).unwrap();
        add_attachment_from_path(&mut source, b"alpha.txt", &path_a).expect("add alpha");
        add_attachment_from_path(&mut source, b"beta.txt", &path_b).expect("add beta");

        // Copy into empty target.
        let mut target = open_minimal();
        let count = copy_attachments_from(&mut target, &mut source, None).expect("copy");
        assert_eq!(count, 2, "must copy 2 attachments");

        // Both must be extractable with correct bytes.
        let got_a = extract_attachment(&mut target, b"alpha.txt").expect("extract alpha");
        let got_b = extract_attachment(&mut target, b"beta.txt").expect("extract beta");
        assert_eq!(got_a, raw_a, "alpha payload must match");
        assert_eq!(got_b, raw_b, "beta payload must match");
    }

    /// Verify that the /Filter chain is preserved verbatim (no recompression).
    #[test]
    fn copy_attachments_filter_and_bytes_preserved() {
        let mut source = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("compressed.txt");
        let raw = b"some data for compression test";
        std::fs::write(&path, raw).unwrap();
        add_attachment_from_path(&mut source, b"compressed.txt", &path).expect("add");

        // Capture the source stream's raw bytes and Filter before copying.
        let src_entries =
            crate::embedded_files::list_embedded_files(&mut source).expect("list source");
        let src_fs_ref = src_entries[0].1;
        let src_stream = resolve_ef_stream(&mut source, src_fs_ref);
        let src_filter = src_stream.dict.get("Filter").cloned();
        let src_data = src_stream.data.clone();

        // Copy to target.
        let mut target = open_minimal();
        copy_attachments_from(&mut target, &mut source, None).expect("copy");

        // Retrieve target stream.
        let tgt_entries =
            crate::embedded_files::list_embedded_files(&mut target).expect("list target");
        assert_eq!(tgt_entries.len(), 1);
        let tgt_fs_ref = tgt_entries[0].1;
        let tgt_stream = resolve_ef_stream(&mut target, tgt_fs_ref);

        // /Filter must be identical.
        assert_eq!(
            tgt_stream.dict.get("Filter").cloned(),
            src_filter,
            "target /Filter must equal source /Filter"
        );
        // Raw bytes must be identical (no re-encode).
        assert_eq!(
            tgt_stream.data, src_data,
            "target stream data must be byte-identical to source"
        );

        // Decoded payload must also match the original file content.
        let decoded =
            crate::filters::decode_stream_data(&tgt_stream.dict, &tgt_stream.data).expect("decode");
        assert_eq!(
            decoded.as_slice(),
            raw.as_ref(),
            "decoded payload must match original"
        );
    }

    /// Verify prefix avoids key collision with a pre-existing target attachment.
    #[test]
    fn copy_attachments_prefix_avoids_collision() {
        let mut source = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");

        // Source has "data.txt".
        let src_path = dir.path().join("data.txt");
        std::fs::write(&src_path, b"source data").unwrap();
        add_attachment_from_path(&mut source, b"data.txt", &src_path).expect("add source");

        // Target also has "data.txt" (different content).
        let mut target = open_minimal();
        let tgt_path = dir.path().join("target_data.txt");
        let original_target_content = b"target original data";
        std::fs::write(&tgt_path, original_target_content).unwrap();
        add_attachment_from_path(&mut target, b"data.txt", &tgt_path).expect("add target pre");

        // Copy with prefix "src-".
        let count = copy_attachments_from(&mut target, &mut source, Some(b"src-")).expect("copy");
        assert_eq!(count, 1);

        // Both keys must coexist.
        let entries = crate::embedded_files::list_embedded_files(&mut target).expect("list");
        assert_eq!(entries.len(), 2, "both keys must exist");

        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(
            keys.contains(&b"data.txt".as_ref()),
            "original key must survive"
        );
        assert!(
            keys.contains(&b"src-data.txt".as_ref()),
            "prefixed key must be present"
        );

        // Original content must be unchanged.
        let original = extract_attachment(&mut target, b"data.txt").expect("extract original");
        assert_eq!(
            original, original_target_content,
            "original content must be intact"
        );

        // Prefixed entry has the source content.
        let prefixed = extract_attachment(&mut target, b"src-data.txt").expect("extract prefixed");
        assert_eq!(
            prefixed, b"source data",
            "prefixed entry must contain source content"
        );
    }

    /// Return value equals the number of copied attachments.
    #[test]
    fn copy_attachments_return_value_is_count() {
        let mut source = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        for name in &["one.txt", "two.txt", "three.txt"] {
            let p = dir.path().join(name);
            std::fs::write(&p, name.as_bytes()).unwrap();
            add_attachment_from_path(&mut source, name.as_bytes(), &p).expect("add");
        }

        let mut target = open_minimal();
        let count = copy_attachments_from(&mut target, &mut source, None).expect("copy");
        assert_eq!(
            count, 3,
            "return value must equal number of attachments copied"
        );
    }

    /// An empty source returns 0 without modifying the target.
    #[test]
    fn copy_attachments_empty_source_returns_zero() {
        let mut source = open_minimal(); // no attachments
        let mut target = open_minimal();

        let count = copy_attachments_from(&mut target, &mut source, None).expect("copy");
        assert_eq!(count, 0, "empty source must return 0");

        let entries = crate::embedded_files::list_embedded_files(&mut target).expect("list");
        assert_eq!(entries.len(), 0, "target must remain empty");
    }

    /// Opening an encrypted source with password then copying returns 0 (the
    /// encrypted fixture has no attachments) without error. This validates that
    /// the password-open → copy pipeline works correctly; a count-based bytes
    /// test is not possible with these fixtures since they carry no attachments.
    ///
    /// Fixture: `tests/fixtures/encrypted/v4-aes-128-r4.pdf`
    /// User password: `user-v4-aes` (AES-128, no weak-crypto flag required for V4/AES).
    #[test]
    fn copy_attachments_encrypted_source_password_open() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../..",
            "/tests/fixtures/encrypted/v4-aes-128-r4.pdf"
        );
        // The encrypted fixture is committed to the repo, so a missing file is
        // a real regression — fail loudly instead of silently skipping, which
        // could turn this into a false-positive pass (CodeRabbit).
        let file = std::fs::File::open(path)
            .expect("encrypted fixture missing: tests/fixtures/encrypted/v4-aes-128-r4.pdf");
        let opts = crate::PdfOpenOptions {
            password: b"user-v4-aes".to_vec(),
            ..crate::PdfOpenOptions::default()
        };
        let mut source = crate::Pdf::open_with_options(std::io::BufReader::new(file), opts)
            .expect("open encrypted source");

        let mut target = open_minimal();
        // The fixture has no attachments; expect 0, no error.
        let count =
            copy_attachments_from(&mut target, &mut source, None).expect("copy from encrypted");
        assert_eq!(
            count, 0,
            "encrypted fixture has no attachments; copy must return 0"
        );
    }

    // ── sanitize_imported_object: foreign indirect references ────────────────

    /// Recursively assert an object graph contains no `Object::Reference`.
    fn assert_no_refs(o: &Object, ctx: &str) {
        match o {
            Object::Reference(r) => panic!("unexpected indirect reference {r} in {ctx}"),
            Object::Array(a) => a.iter().for_each(|e| assert_no_refs(e, ctx)),
            Object::Dictionary(d) => d.iter().for_each(|(_, v)| assert_no_refs(v, ctx)),
            Object::Stream(s) => s.dict.iter().for_each(|(_, v)| assert_no_refs(v, ctx)),
            _ => {}
        }
    }

    /// Build a `source` whose `/EmbeddedFile` stream dict carries foreign
    /// indirect references: an indirect `/Params` dict that *itself* holds an
    /// indirect `/CreationDate`, an indirect `/Filter`, and a dangling ref to
    /// a non-existent object. After `copy_attachments_from` the target copy
    /// must contain zero `Object::Reference`s, the dangling key must be gone,
    /// and the payload must still decode.
    #[test]
    fn copy_attachments_sanitizes_foreign_and_dangling_refs() {
        let mut source = open_minimal();

        let base = source
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let date_ref = ObjectRef::new(base + 1, 0);
        let params_ref = ObjectRef::new(base + 2, 0);
        let filter_ref = ObjectRef::new(base + 3, 0);
        let stream_ref = ObjectRef::new(base + 4, 0);
        let fs_ref = ObjectRef::new(base + 5, 0);
        let dangling = ObjectRef::new(base + 999, 0); // never set

        source.set_object(date_ref, Object::String(b"D:20260101000000Z".to_vec()));
        let mut params = Dictionary::new();
        params.insert("Size", Object::Integer(5));
        params.insert("CreationDate", Object::Reference(date_ref)); // nested foreign ref
        source.set_object(params_ref, Object::Dictionary(params));
        source.set_object(filter_ref, Object::Name(b"FlateDecode".to_vec()));

        // FlateDecode-compress the payload so /Filter must be inlined for decode.
        let raw = b"hello sanitize";
        let mut enc_dict = Dictionary::new();
        enc_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let encoded = encode_stream_data(&enc_dict, raw).expect("encode");

        let mut sdict = Dictionary::new();
        sdict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        sdict.insert("Filter", Object::Reference(filter_ref)); // foreign /Filter ref
        sdict.insert("Params", Object::Reference(params_ref)); // foreign /Params ref
        sdict.insert("Length", Object::Integer(encoded.len() as i64));
        sdict.insert("DanglingKey", Object::Reference(dangling)); // unresolvable
        source.set_object(stream_ref, Object::Stream(Stream::new(sdict, encoded)));

        let mut efsub = Dictionary::new();
        efsub.insert("F", Object::Reference(stream_ref));
        efsub.insert("UF", Object::Reference(stream_ref));
        let mut fs = Dictionary::new();
        fs.insert("Type", Object::Name(b"Filespec".to_vec()));
        fs.insert("F", Object::String(b"s.bin".to_vec()));
        fs.insert("UF", Object::String(b"s.bin".to_vec()));
        fs.insert("EF", Object::Dictionary(efsub));
        source.set_object(fs_ref, Object::Dictionary(fs));
        insert_embedded_file(&mut source, b"s.bin", fs_ref).expect("insert source");

        let mut target = open_minimal();
        let n = copy_attachments_from(&mut target, &mut source, None).expect("copy");
        assert_eq!(n, 1);

        let tgt_entries = list_embedded_files(&mut target).expect("list target");
        assert_eq!(tgt_entries.len(), 1);
        let tgt_fs_ref = tgt_entries[0].1;

        // The target filespec dict must hold no foreign refs except the /EF
        // entries (which point at the freshly-allocated target stream).
        let Object::Dictionary(tgt_fs) = target.resolve(tgt_fs_ref).expect("resolve tgt fs") else {
            panic!("filespec not a dict");
        };
        for (k, v) in tgt_fs.iter() {
            if k == b"EF" {
                continue; // /EF intentionally references the target stream
            }
            assert_no_refs(v, "target filespec (non-/EF)");
        }

        let tgt_stream = resolve_ef_stream(&mut target, tgt_fs_ref);
        // Every foreign ref in the stream dict must be inlined or dropped.
        assert_no_refs(
            &Object::Dictionary(tgt_stream.dict.clone()),
            "target stream dict",
        );
        // /Params inlined as a dict, with the nested date inlined as a String.
        match tgt_stream.dict.get("Params") {
            Some(Object::Dictionary(p)) => {
                assert_eq!(
                    p.get("CreationDate").cloned(),
                    Some(Object::String(b"D:20260101000000Z".to_vec())),
                    "nested /CreationDate must be inlined"
                );
            }
            other => panic!("/Params must be an inlined dict, got {other:?}"),
        }
        // /Filter inlined as a Name so the payload still decodes.
        assert_eq!(
            tgt_stream.dict.get("Filter").cloned(),
            Some(Object::Name(b"FlateDecode".to_vec())),
            "/Filter must be inlined as a Name"
        );
        // The unresolvable /DanglingKey must no longer be an indirect
        // reference. Per ISO 32000-1, a reference to a non-existent object IS
        // null, so `source.resolve` yields `Null`; inlining that null (or
        // dropping the key) is correct — what matters is that no foreign
        // `Object::Reference` survives into the target.
        match tgt_stream.dict.get("DanglingKey") {
            None | Some(Object::Null) => {}
            other => panic!("unresolvable ref must become null/absent, got {other:?}"),
        }
        // Payload still decodes to the original bytes.
        let decoded =
            crate::filters::decode_stream_data(&tgt_stream.dict, &tgt_stream.data).expect("decode");
        assert_eq!(decoded.as_slice(), raw.as_ref());
    }
}
