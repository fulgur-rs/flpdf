//! Structured enumeration and formatted display of PDF attachments.
//!
//! This module builds on [`crate::embedded_files::list_embedded_files`] and
//! [`crate::filespec_helper::FileSpec`] to produce a structured [`AttachmentInfo`]
//! per attachment, and a human-readable formatted listing via
//! [`format_attachment_list`].
//!
//! # Output format
//!
//! The format is modelled on `qpdf --list-attachments [--verbose]`:
//!
//! ```text
//! key -> num,gen
//!   display name: <name or (none)>
//!   size:         <n or (none)>
//!   mime type:    <type or (none)>
//!   creation date:     <date or (none)>
//!   modification date: <date or (none)>
//! ```
//!
//! With `verbose: true`, three additional lines are appended per attachment:
//!
//! ```text
//!   description:      <desc or (none)>
//!   af relationship:  <rel or (none)>
//!   checksum:         <hex or (none)>
//! ```
//!
//! # Missing fields
//!
//! All missing or absent fields are displayed as `(none)` rather than left empty.
//!
//! # Example
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{attachment_list, Pdf};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachments.pdf")?))?;
//! let infos = attachment_list::list_attachment_info(&mut pdf)?;
//! print!("{}", attachment_list::format_attachment_list(&infos, false));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::embedded_files::list_embedded_files;
use crate::filespec_helper::FileSpec;
use crate::{ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

// ── AttachmentInfo ────────────────────────────────────────────────────────────

/// Structured metadata for a single PDF attachment.
///
/// Fields are `Option<Vec<u8>>` (or `Option<i64>` for `size`) because any
/// field may be absent in a well-formed or partially-formed PDF.  The
/// formatter renders absent fields as `(none)`.
///
/// `key` and `filespec_ref` are always present — they come from the
/// `/Names /EmbeddedFiles` name tree and are required to have found the entry
/// in the first place.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentInfo {
    /// Raw name-tree key (the bytes used to look up this attachment).
    pub key: Vec<u8>,
    /// Object reference of the `/Filespec` dictionary.
    pub filespec_ref: ObjectRef,
    /// Display name: decoded `/UF` (preferred) or decoded `/F`.  `None` when
    /// both are absent.
    pub display_name: Option<String>,
    /// Uncompressed file size from `/Params /Size`.
    pub size: Option<i64>,
    /// MIME type from `/EmbeddedFile /Subtype` (raw bytes from PDF Name).
    pub mimetype: Option<Vec<u8>>,
    /// Raw PDF date string from `/Params /CreationDate`.
    pub creation_date: Option<Vec<u8>>,
    /// Raw PDF date string from `/Params /ModDate`.
    pub modification_date: Option<Vec<u8>>,
    // ── verbose-only fields ───────────────────────────────────────────────
    /// Human-readable description from `/Filespec /Desc`.
    pub description: Option<Vec<u8>>,
    /// Associated-file relationship from `/Filespec /AFRelationship`.
    pub af_relationship: Option<Vec<u8>>,
    /// MD5 checksum from `/Params /CheckSum` (raw bytes; displayed as hex).
    pub checksum: Option<Vec<u8>>,
}

// ── UTF-16BE decoder ──────────────────────────────────────────────────────────

/// Decode a PDF text string (ISO 32000-1 §7.9.2) to a UTF-8 `String` for
/// display.
///
/// Delegates to the single canonical decoder
/// [`crate::json_inspect::decode_pdf_text_string`], which handles UTF-16BE /
/// UTF-16LE BOM-prefixed strings **and** the full PDFDocEncoding table
/// (ISO 32000-1 Annex D.3) — so non-ASCII `/F`, `/UF`, and `/Desc` values are
/// decoded correctly instead of being mangled by a lossy UTF-8 fallback
/// (roborev #953).  If the bytes cannot be interpreted as a PDF text string,
/// fall back to lossy UTF-8 so the listing still shows *something* rather than
/// failing.
fn decode_pdf_text_string(bytes: &[u8]) -> String {
    crate::json_inspect::decode_pdf_text_string(bytes)
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

// ── list_attachment_info ──────────────────────────────────────────────────────

/// Enumerate all attachments in `pdf` and return their structured metadata.
///
/// Iterates the `/Names /EmbeddedFiles` name tree via
/// [`list_embedded_files`], then reads each `/Filespec` dictionary and its
/// associated `/EmbeddedFile` stream for metadata.
///
/// An empty list is returned — without error — when the document has no
/// embedded files.
///
/// For a runnable walkthrough see `examples/pull_attachments.rs`.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`] or the embedded-files walker.
pub fn list_attachment_info<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<AttachmentInfo>> {
    let entries = list_embedded_files(pdf)?;
    let mut out = Vec::with_capacity(entries.len());

    for (key, filespec_ref) in entries {
        let info = collect_one(pdf, key, filespec_ref)?;
        out.push(info);
    }

    Ok(out)
}

/// Build an [`AttachmentInfo`] for a single `(key, filespec_ref)` pair.
fn collect_one<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    key: Vec<u8>,
    filespec_ref: ObjectRef,
) -> Result<AttachmentInfo> {
    let mut fs = FileSpec::new(filespec_ref, pdf);

    // ── Display name: /UF preferred, fall back to /F ─────────────────────
    let display_name: Option<String> = {
        let uf_raw = fs.uf()?;
        let f_raw = fs.filename()?;
        match (uf_raw, f_raw) {
            (Some(uf), _) => Some(decode_pdf_text_string(&uf)),
            (None, Some(f)) => Some(decode_pdf_text_string(&f)),
            (None, None) => None,
        }
    };

    // ── Verbose fields from /Filespec ─────────────────────────────────────
    let description = fs.description()?;
    let af_relationship = fs.af_relationship()?;

    // ── Metadata from /EmbeddedFile stream ────────────────────────────────
    let (size, mimetype, creation_date, modification_date, checksum) = match fs.embedded_file()? {
        Some(ef) => (
            ef.size()?,
            ef.mimetype()?,
            ef.creation_date()?,
            ef.modification_date()?,
            ef.checksum()?,
        ),
        None => (None, None, None, None, None),
    };

    Ok(AttachmentInfo {
        key,
        filespec_ref,
        display_name,
        size,
        mimetype,
        creation_date,
        modification_date,
        description,
        af_relationship,
        checksum,
    })
}

// ── format_attachment_list ────────────────────────────────────────────────────

/// Format a slice of [`AttachmentInfo`] entries as a human-readable string.
///
/// When `verbose` is `false`, each attachment is rendered as:
///
/// ```text
/// key -> num,gen
///   display name: <name or (none)>
///   size:         <n or (none)>
///   mime type:    <type or (none)>
///   creation date:     <date or (none)>
///   modification date: <date or (none)>
/// ```
///
/// When `verbose` is `true`, three additional lines follow:
///
/// ```text
///   description:     <desc or (none)>
///   af relationship: <rel or (none)>
///   checksum:        <hex or (none)>
/// ```
///
/// All absent/missing fields are rendered as the literal string `(none)`.
/// Dates are printed as-is (raw PDF date string).
///
/// # Note on qpdf wording
///
/// The field labels (`display name:`, `mime type:`, etc.) are modelled on
/// `qpdf --list-attachments --verbose` output.  The per-field multi-line layout
/// differs from qpdf's non-verbose single-line `key -> num,gen` format: the
/// header line is shared but plain mode adds per-field lines, which qpdf omits.
/// CLI task .10.9 may adjust final output wording further.
/// The checksum is formatted as lowercase hexadecimal, matching qpdf output.
pub fn format_attachment_list(entries: &[AttachmentInfo], verbose: bool) -> String {
    let mut out = String::new();
    for info in entries {
        // Header line: key -> num,gen  (mirrors qpdf).  The name-tree key is a
        // PDF string; decode it the same way as display name / description so
        // non-ASCII PDFDocEncoding / UTF-16BE keys are not mojibake (roborev
        // #954).  Only the displayed text is decoded — `info.key` keeps its
        // raw bytes for lookups elsewhere.
        let key_str = decode_pdf_text_string(&info.key);
        out.push_str(&format!(
            "{} -> {},{}\n",
            key_str, info.filespec_ref.number, info.filespec_ref.generation
        ));

        // Display name
        let name_s = info
            .display_name
            .as_deref()
            .map(|s| s.to_owned())
            .unwrap_or_else(|| "(none)".to_owned());
        out.push_str(&format!("  display name: {name_s}\n"));

        // Size
        let size_s = info
            .size
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(none)".to_owned());
        out.push_str(&format!("  size:         {size_s}\n"));

        // MIME type
        let mime_s = info
            .mimetype
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "(none)".to_owned());
        out.push_str(&format!("  mime type:    {mime_s}\n"));

        // Creation date
        let cdate_s = info
            .creation_date
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "(none)".to_owned());
        out.push_str(&format!("  creation date:     {cdate_s}\n"));

        // Modification date
        let mdate_s = info
            .modification_date
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "(none)".to_owned());
        out.push_str(&format!("  modification date: {mdate_s}\n"));

        if verbose {
            // Description — `/Filespec /Desc` is a PDF text string, so decode
            // it through the canonical PDFDocEncoding/UTF-16 decoder rather
            // than a lossy UTF-8 cast (roborev #953).
            let desc_s = info
                .description
                .as_deref()
                .map(decode_pdf_text_string)
                .unwrap_or_else(|| "(none)".to_owned());
            out.push_str(&format!("  description:     {desc_s}\n"));

            // AFRelationship
            let rel_s = info
                .af_relationship
                .as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_else(|| "(none)".to_owned());
            out.push_str(&format!("  af relationship: {rel_s}\n"));

            // Checksum as lowercase hex
            let chk_s = info
                .checksum
                .as_deref()
                .map(|b| {
                    b.iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                })
                .unwrap_or_else(|| "(none)".to_owned());
            out.push_str(&format!("  checksum:        {chk_s}\n"));
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedded_files::insert_embedded_file;
    use crate::filespec_helper::{
        encode_utf16be, format_pdf_date, FileParamDates, FileSpecBuilder,
    };
    use crate::Pdf;
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

    // ── Fixture PDF with actual attachment ────────────────────────────────────

    #[test]
    fn fixture_attachment_two_page_returns_one_or_more() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/compat/attachment-two-page.pdf"
        );
        let f = std::fs::File::open(path);
        if f.is_err() {
            // Fixture absent — skip gracefully.
            return;
        }
        let mut pdf = Pdf::open(std::io::BufReader::new(f.unwrap())).expect("open fixture");
        let infos = list_attachment_info(&mut pdf).expect("list");
        assert!(
            !infos.is_empty(),
            "fixture must have at least one attachment"
        );
        // First entry must have a display name
        let first = &infos[0];
        assert!(
            first.display_name.is_some(),
            "display_name must be present for fixture attachment"
        );
    }

    // ── Empty document → empty list ───────────────────────────────────────────

    #[test]
    fn empty_document_returns_empty_list() {
        let mut pdf = open_minimal();
        let infos = list_attachment_info(&mut pdf).expect("list");
        assert!(infos.is_empty(), "no attachments → empty list");
    }

    // ── (none) for missing metadata fields ───────────────────────────────────

    #[test]
    fn missing_metadata_renders_as_none() {
        let mut pdf = open_minimal();

        // Build a filespec with no mimetype, no dates, no desc, no AFRelationship.
        let fs_ref = FileSpecBuilder::new("bare.txt", b"payload")
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"bare.txt", fs_ref).expect("insert");

        let infos = list_attachment_info(&mut pdf).expect("list");
        assert_eq!(infos.len(), 1);
        let info = &infos[0];

        // The formatted non-verbose output must contain (none) for each absent field.
        let formatted = format_attachment_list(std::slice::from_ref(info), false);
        assert!(
            formatted.contains("mime type:    (none)"),
            "missing mime must render as (none): {formatted:?}"
        );
        assert!(
            formatted.contains("creation date:     (none)"),
            "missing creation date must render as (none): {formatted:?}"
        );
        assert!(
            formatted.contains("modification date: (none)"),
            "missing mod date must render as (none): {formatted:?}"
        );

        // Verbose: desc, af_relationship, checksum also (none)
        // Note: FileSpecBuilder always writes /Params /CheckSum, so we only test desc/af.
        let formatted_v = format_attachment_list(std::slice::from_ref(info), true);
        assert!(
            formatted_v.contains("description:     (none)"),
            "missing desc must render as (none): {formatted_v:?}"
        );
        assert!(
            formatted_v.contains("af relationship: (none)"),
            "missing af_rel must render as (none): {formatted_v:?}"
        );
    }

    // ── verbose adds extra fields ─────────────────────────────────────────────

    #[test]
    fn verbose_adds_desc_af_checksum_lines() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("verbose.txt", b"some data")
            .description(b"A description")
            .af_relationship(b"Source")
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"verbose.txt", fs_ref).expect("insert");

        let infos = list_attachment_info(&mut pdf).expect("list");
        assert_eq!(infos.len(), 1);

        let non_verbose = format_attachment_list(&infos, false);
        let verbose = format_attachment_list(&infos, true);

        // Non-verbose must NOT contain description / af relationship / checksum lines
        assert!(
            !non_verbose.contains("description:"),
            "non-verbose must not include description: {non_verbose:?}"
        );
        assert!(
            !non_verbose.contains("af relationship:"),
            "non-verbose must not include af relationship: {non_verbose:?}"
        );
        assert!(
            !non_verbose.contains("checksum:"),
            "non-verbose must not include checksum: {non_verbose:?}"
        );

        // Verbose must include all three extra fields
        assert!(
            verbose.contains("description:     A description"),
            "verbose must include description: {verbose:?}"
        );
        assert!(
            verbose.contains("af relationship: Source"),
            "verbose must include af relationship: {verbose:?}"
        );
        assert!(
            verbose.contains("checksum:"),
            "verbose must include checksum line: {verbose:?}"
        );
    }

    // ── /UF absent → /F used as display name ─────────────────────────────────

    #[test]
    fn f_only_filespec_uses_f_as_display_name() {
        use crate::object::{Dictionary, Object, Stream};
        use crate::ObjectRef;

        let mut pdf = open_minimal();

        // Allocate objects manually: stream + filespec with /F only (no /UF).
        let next = pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let stream_ref = ObjectRef::new(next + 1, 0);
        let filespec_ref = ObjectRef::new(next + 2, 0);

        // Minimal EmbeddedFile stream (no /Params → missing size/dates/checksum).
        let mut ef_dict = Dictionary::new();
        ef_dict.insert("Type", Object::Name(b"EmbeddedFile".to_vec()));
        ef_dict.insert("Length", Object::Integer(4));
        let ef_stream = Stream::new(ef_dict, b"data".to_vec());
        pdf.set_object(stream_ref, Object::Stream(ef_stream));

        // /EF sub-dict with only /F.
        let mut ef_sub = Dictionary::new();
        ef_sub.insert("F", Object::Reference(stream_ref));

        // Filespec with /F only (no /UF).
        let mut fs_dict = Dictionary::new();
        fs_dict.insert("Type", Object::Name(b"Filespec".to_vec()));
        fs_dict.insert("F", Object::String(b"only-f.txt".to_vec()));
        fs_dict.insert("EF", Object::Dictionary(ef_sub));
        pdf.set_object(filespec_ref, Object::Dictionary(fs_dict));

        insert_embedded_file(&mut pdf, b"only-f.txt", filespec_ref).expect("insert");

        let infos = list_attachment_info(&mut pdf).expect("list");
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].display_name.as_deref(),
            Some("only-f.txt"),
            "/F must be used as display name when /UF is absent"
        );

        let formatted = format_attachment_list(&infos, false);
        assert!(
            formatted.contains("display name: only-f.txt"),
            "formatted output must show /F as display name: {formatted:?}"
        );
    }

    // ── /UF (UTF-16BE) is decoded correctly ──────────────────────────────────

    #[test]
    fn uf_utf16be_is_decoded_for_display() {
        let mut pdf = open_minimal();

        // FileSpecBuilder writes /UF as UTF-16BE.
        let fs_ref = FileSpecBuilder::new("hello.txt", b"hi")
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"hello.txt", fs_ref).expect("insert");

        let infos = list_attachment_info(&mut pdf).expect("list");
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].display_name.as_deref(),
            Some("hello.txt"),
            "/UF must decode to the original filename"
        );
    }

    // ── checksum is hex-encoded ───────────────────────────────────────────────

    #[test]
    fn checksum_displayed_as_lowercase_hex() {
        let mut pdf = open_minimal();

        let payload = b"checksum test";
        let fs_ref = FileSpecBuilder::new("chk.txt", payload.as_ref())
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"chk.txt", fs_ref).expect("insert");

        let infos = list_attachment_info(&mut pdf).expect("list");
        assert_eq!(infos.len(), 1);

        let verbose = format_attachment_list(&infos, true);
        // The checksum line must contain lowercase hex, not raw bytes.
        // The actual MD5 of b"checksum test" is deterministic; verify format.
        let chk_line = verbose
            .lines()
            .find(|l| l.trim_start().starts_with("checksum:"))
            .expect("checksum line must be present");
        let hex_part = chk_line.split(':').nth(1).unwrap_or("").trim();
        // Must be all hex digits (32 chars for MD5).
        assert!(
            hex_part.len() == 32 && hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "checksum must be 32-char lowercase hex: {hex_part:?}"
        );
        assert!(
            hex_part == hex_part.to_lowercase(),
            "checksum must be lowercase: {hex_part:?}"
        );
    }

    // ── dates and full metadata with FileSpecBuilder ──────────────────────────

    #[test]
    fn full_metadata_attachment() {
        let mut pdf = open_minimal();

        let dates = FileParamDates {
            creation: Some((2026, 1, 1, 0, 0, 0)),
            modification: Some((2026, 6, 15, 12, 30, 0)),
        };
        let fs_ref = FileSpecBuilder::new("full.txt", b"full payload")
            .mimetype(b"text/plain")
            .description(b"Full test attachment")
            .af_relationship(b"Data")
            .dates(dates)
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"full.txt", fs_ref).expect("insert");

        let infos = list_attachment_info(&mut pdf).expect("list");
        assert_eq!(infos.len(), 1);
        let info = &infos[0];

        assert_eq!(info.key, b"full.txt");
        assert_eq!(info.display_name.as_deref(), Some("full.txt"));
        assert_eq!(info.size, Some(12));
        assert_eq!(
            info.mimetype.as_deref(),
            Some(b"text/plain".as_ref()),
            "mimetype must match"
        );
        assert_eq!(
            info.creation_date.as_deref(),
            Some(format_pdf_date(2026, 1, 1, 0, 0, 0).as_slice()),
        );
        assert_eq!(
            info.modification_date.as_deref(),
            Some(format_pdf_date(2026, 6, 15, 12, 30, 0).as_slice()),
        );
        assert_eq!(
            info.description.as_deref(),
            Some(b"Full test attachment".as_ref())
        );
        assert_eq!(info.af_relationship.as_deref(), Some(b"Data".as_ref()));
        assert!(info.checksum.is_some(), "checksum must be present");

        // Format check
        let formatted = format_attachment_list(std::slice::from_ref(info), true);
        assert!(formatted.contains("mime type:    text/plain"));
        assert!(formatted.contains("creation date:     D:20260101000000Z"));
        assert!(formatted.contains("modification date: D:20260615123000Z"));
        assert!(formatted.contains("description:     Full test attachment"));
        assert!(formatted.contains("af relationship: Data"));
    }

    // ── decode_pdf_text_string ────────────────────────────────────────────────

    #[test]
    fn decode_utf16be_bom() {
        let bytes = encode_utf16be("hello");
        assert_eq!(decode_pdf_text_string(&bytes), "hello");
    }

    #[test]
    fn decode_ascii_fallback() {
        let bytes = b"plain ascii".to_vec();
        assert_eq!(decode_pdf_text_string(&bytes), "plain ascii");
    }

    // Regression for roborev #953: non-ASCII PDFDocEncoding must decode via
    // the canonical ISO 32000-1 Annex D.3 table, not a lossy UTF-8 cast.
    #[test]
    fn decode_non_ascii_pdfdocencoding() {
        // 0x18 → U+02D8 BREVE (a PDF-specific PDFDocEncoding code point).
        assert_eq!(decode_pdf_text_string(&[0x18]), "\u{02D8}");
        // 0xE9 → U+00E9 'é' (PDFDocEncoding follows ISO-8859-1 in 0xA0..=0xFF).
        assert_eq!(decode_pdf_text_string(&[0xE9]), "é");
        // Mixed ASCII + non-ASCII PDFDocEncoding round-trips correctly rather
        // than emitting U+FFFD replacement characters.
        assert_eq!(decode_pdf_text_string(b"caf\xE9"), "café");
    }

    // Regression for roborev #954: the header line key must be decoded as a
    // PDF text string, not lossy UTF-8, so non-ASCII keys are not mojibake.
    #[test]
    fn header_key_decodes_pdfdocencoding() {
        let info = AttachmentInfo {
            key: b"caf\xE9.txt".to_vec(),
            filespec_ref: crate::ObjectRef::new(7, 0),
            display_name: Some("café.txt".to_owned()),
            size: None,
            mimetype: None,
            creation_date: None,
            modification_date: None,
            description: None,
            af_relationship: None,
            checksum: None,
        };
        let formatted = format_attachment_list(std::slice::from_ref(&info), false);
        assert!(
            formatted.contains("café.txt -> 7,0"),
            "non-ASCII PDFDocEncoding key must decode in the header: {formatted:?}"
        );
        assert!(
            !formatted.contains('\u{FFFD}'),
            "header must not contain replacement chars: {formatted:?}"
        );
    }

    // Regression for roborev #953: /Desc verbose output must decode PDF text
    // strings (here UTF-16BE) instead of showing mojibake.
    #[test]
    fn verbose_description_decodes_utf16be() {
        let info = AttachmentInfo {
            key: b"k.txt".to_vec(),
            filespec_ref: crate::ObjectRef::new(1, 0),
            display_name: Some("k.txt".to_owned()),
            size: None,
            mimetype: None,
            creation_date: None,
            modification_date: None,
            description: Some(encode_utf16be("dé")),
            af_relationship: None,
            checksum: None,
        };
        let formatted = format_attachment_list(std::slice::from_ref(&info), true);
        assert!(
            formatted.contains("description:     dé"),
            "UTF-16BE /Desc must be decoded: {formatted:?}"
        );
    }
}
