//! Ports qpdf's `test_34` through `test_41` (`qpdf/test_driver.cc:1252-1404`
//! in pinned qpdf 11.9.0).
//!
//! House style, shared helpers (`resolve_chain`, `write_object`,
//! `write_qpdf_object`), and the repair-diagnostics threading convention
//! all follow `driver/test_0_1.rs` -- read that file first.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::{
    DecodeLevel, Error, ObjectHandle, PageDocumentHelper, PageObjectHelper, Pdf, PdfWriter,
    TokenFilter, TokenFilterOutput,
};

use super::emit_new_diagnostics;
use crate::output::write_bytes;

// ---------------------------------------------------------------------------
// Shared helpers
//
// qpdf's own C++ accessors (`getKey`, `getArrayItem`, `unparseResolved`, ...)
// transparently dereference an unresolved indirect handle on every call
// (`QPDFObjectHandle::dereference`, `libqpdf/QPDFObjectHandle.cc:2376-2383`).
// This crate's `ObjectHandle` accessors deliberately do not (see e.g.
// `ObjectHandle::get_key`'s own doc: "Never performs resolution itself"), so
// every qpdf `getKey(...)`/array-item step that is followed by a further
// accessor call needs an explicit chase here to observe the same value qpdf
// would. `resolve_to_terminal` (not the single-hop
// `resolve`) is used throughout, matching the precedent
// `driver/test_0_1.rs` itself established (see that file's own doc on
// `resolve_to_terminal_ref`): it degrades to a plain one-hop
// resolve for ordinary parsed PDF content (where a resolved object's own
// value is never itself `ObjectValue::Reference` -- that state is reachable
// only through this crate's own `Pdf::set_object` test seam, per
// `ObjectHandle::type_code`'s own doc), while still matching qpdf's
// self-referential-object cycle handling (`libqpdf/QPDF.cc:1699-1712`) for
// the pathological case where it is.
// ---------------------------------------------------------------------------

/// Resolve `handle` to its terminal value and drain any repair diagnostics
/// that dereference produced, mirroring qpdf's synchronous per-dereference
/// warning emission to stderr.
fn resolved_terminal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<ObjectHandle> {
    let resolved = pdf.resolve_to_terminal(handle)?;
    emit_new_diagnostics(pdf, diagnostics_written, filename, stdout, stderr)?;
    Ok(resolved)
}

/// `handle.getKey(key)` plus the implicit dereference of the *returned*
/// child that qpdf's next accessor call on it would perform
/// (`QPDFObjectHandle::getKey`, `libqpdf/QPDFObjectHandle.cc:979-988`).
#[allow(clippy::too_many_arguments)]
fn resolved_key<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    handle: &ObjectHandle,
    key: &[u8],
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<ObjectHandle> {
    let child = handle.get_key(key);
    resolved_terminal(pdf, &child, filename, diagnostics_written, stdout, stderr)
}

/// `QPDF::getRoot()`'s minimal path (`libqpdf/QPDF.cc:2354-2368`): fetch
/// `/Root` from the trailer and error if it is not a dictionary. The
/// `check_mode` `/Type /Catalog` repair branch there is qpdf's `--check`-only
/// behavior and this driver never runs in that mode, so it has no
/// counterpart here.
fn root_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<ObjectHandle> {
    let candidate = pdf.trailer_key_handle(b"Root");
    let resolved = resolved_terminal(
        pdf,
        &candidate,
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    if resolved.as_dictionary().is_none() {
        return Err(Error::System("unable to find /Root dictionary".to_string()));
    }
    Ok(resolved)
}

/// Mirrors `QPDF::getVersionAsPDFVersion`'s leading-prefix regex match
/// (`std::regex` `^[[:space:]]*([0-9]+)\.([0-9]+)`, `libqpdf/QPDF.cc:2305-2320`):
/// skip leading whitespace, take the leading run of digits, a literal `.`,
/// and the following run of digits; default to major 1, minor 3 if the
/// header string doesn't start that way. This does not use
/// `flpdf::PdfVersion::parse`: that method requires the *entire* string to
/// be exactly `M.m` (via `str::split_once` + full-string integer parses),
/// while qpdf's regex only needs a matching *prefix* and tolerates leading
/// whitespace -- a real divergence for a malformed header, not just a
/// stylistic difference.
fn version_prefix_major_minor(version: &str) -> (i64, i64) {
    let bytes = version.as_bytes();
    let mut index = 0;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    let major_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == major_start || bytes.get(index) != Some(&b'.') {
        return (1, 3);
    }
    let major_str = &version[major_start..index];
    index += 1; // skip '.'
    let minor_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == minor_start {
        return (1, 3);
    }
    let minor_str = &version[minor_start..index];
    // qpdf's `QUtil::string_to_int` throws on overflow (an uncaught
    // exception, since these captured groups are pure ASCII digits with no
    // sign); a version header with a digit run wide enough to overflow
    // `i64` is not exercised by any fixture this driver ships, so this
    // falls back rather than aborting the process.
    let major = major_str.parse::<i64>().unwrap_or(1);
    let minor = minor_str.parse::<i64>().unwrap_or(3);
    (major, minor)
}

/// `QPDF::getExtensionLevel` (`libqpdf/QPDF.cc:2328-2346`): walk
/// `/Extensions /ADBE /ExtensionLevel` from `root`, resolving indirect
/// references at each step, defaulting to `0` whenever a link in that chain
/// is absent or the wrong type. This does not reuse `Pdf::adobe_extension_level`:
/// that convenience method starts from `self.trailer_dictionary().get_ref("Root")`,
/// which requires `/Root` to be a *literal* `Object::Reference` in the
/// trailer and silently returns `None` (defaulting to `0`) for a direct
/// `/Root` dictionary -- unlike qpdf's own `getRoot()`, and unlike
/// `root_handle` above, both of which accept `/Root` either way. Using the
/// convenience method here would make `run_test_34` internally
/// inconsistent: `extension level: 0` from a direct `/Root` on the same
/// line whose very next line prints a real, non-empty `/Extensions`
/// dictionary read through `root_handle`.
#[allow(clippy::too_many_arguments)]
fn catalog_extension_level<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    root: &ObjectHandle,
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<i64> {
    let extensions = resolved_key(
        pdf,
        root,
        b"/Extensions",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    if extensions.as_dictionary().is_none() {
        return Ok(0);
    }
    let adbe = resolved_key(
        pdf,
        &extensions,
        b"/ADBE",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    if adbe.as_dictionary().is_none() {
        return Ok(0);
    }
    let level = resolved_key(
        pdf,
        &adbe,
        b"/ExtensionLevel",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    // qpdf reads this with `getIntValueAsInt()`, which clamps an
    // out-of-`int`-range value to `INT_MIN`/`INT_MAX` (and warns) rather
    // than keeping the full 64-bit value (`QPDFObjectHandle::getIntValueAsInt`,
    // `libqpdf/QPDFObjectHandle.cc:527-542`); the clamp is reproduced here,
    // but the accompanying `warnIfPossible` is not -- see this file's own
    // caveats.
    Ok(level
        .as_integer()
        .map(|value| value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)))
        .unwrap_or(0))
}

/// `QUtil::hex_encode` (`libqpdf/QUtil.cc:720-731`): lowercase hex, two
/// characters per byte, no separators.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ---------------------------------------------------------------------------
// test_34 (`test_driver.cc:1251-1263`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_34<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    writeln!(stdout, "version: {}", pdf.version())?;

    // qpdf's `getExtensionLevel()` calls `getRoot()` on its own
    // (`libqpdf/QPDF.cc:2332`), and the direct `getKey("/Extensions")` below
    // is a *second*, independent `getRoot()` call (`test_driver.cc:1257`);
    // reusing one resolved `root_handle` for both is behavior-equivalent
    // (same canonical object either way) without spending the resolve twice.
    let root = root_handle(pdf, filename, diagnostics_written, stdout, stderr)?;
    let extension_level =
        catalog_extension_level(pdf, &root, filename, diagnostics_written, stdout, stderr)?;
    writeln!(stdout, "extension level: {extension_level}")?;

    // `getKey("/Extensions").unparse()` never dereferences: an indirect
    // result always prints its own `N G R` regardless of resolution state,
    // and a direct result's own nested children print the same way
    // (`ObjectHandle::unparse`'s own doc) -- no extra resolve step here.
    let extensions = root.get_key(b"/Extensions");
    write_bytes(stdout, &extensions.unparse())?;
    writeln!(stdout)?;

    let (major, minor) = version_prefix_major_minor(pdf.version());
    writeln!(stdout, "As PDFVersion: {major}.{minor}/{extension_level}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_35 / test_36 (`test_driver.cc:1265-1338`)
//
// qpdf itself writes these as two fully independent functions -- no shared
// private helper, unlike e.g. its own `test_56_59` -- so `run_test_35` and
// `run_test_36` below stay two separate walks with the array/loop structure
// duplicated between them, matching that. `matching_filespec_ef_f_stream`
// below *is* a shared helper, but it factors out only the one five-clause
// boolean predicate the two loop bodies both evaluate byte-for-byte
// identically (`item.isDictionary() && ... .isStream()`, present verbatim
// in both `test_driver.cc:1277-1279` and `:1323-1325`) -- a "same order, no
// behavior change" consolidation (CLAUDE.md's (B) class of deviation, not a
// restructuring of qpdf's own two-function shape).
// ---------------------------------------------------------------------------

/// `item.isDictionary() && item.getKey("/Type").isName() &&
/// (item.getKey("/Type").getName() == "/Filespec") &&
/// item.getKey("/EF").isDictionary() && item.getKey("/EF").getKey("/F").isStream()`
/// (`test_driver.cc:1277-1279`, `1323-1325`), returning the resolved `/EF /F`
/// stream handle on a match. Name *values* (as opposed to dictionary key
/// strings) are decoded without a leading `/` in this crate
/// (`ObjectHandle::as_name`'s own doc; contrast `ObjectHandle::get_key`,
/// which requires the leading `/` for the *key* string), so the comparison
/// below is against `b"Filespec"`, not `b"/Filespec"`.
#[allow(clippy::too_many_arguments)]
fn matching_filespec_ef_f_stream<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    item: &ObjectHandle,
    filename: &[u8],
    diagnostics_written: &mut usize,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Option<ObjectHandle>> {
    if item.as_dictionary().is_none() {
        return Ok(None);
    }
    let type_name = resolved_key(
        pdf,
        item,
        b"/Type",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    if type_name.as_name().as_deref() != Some(b"Filespec".as_slice()) {
        return Ok(None);
    }
    let ef = resolved_key(
        pdf,
        item,
        b"/EF",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    if ef.as_dictionary().is_none() {
        return Ok(None);
    }
    let ef_f = resolved_key(
        pdf,
        &ef,
        b"/F",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    if ef_f.as_stream_dict().is_none() {
        return Ok(None);
    }
    Ok(Some(ef_f))
}

pub(crate) fn run_test_35<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root = root_handle(pdf, filename, diagnostics_written, stdout, stderr)?;
    let names = resolved_key(
        pdf,
        &root,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let embedded_files = resolved_key(
        pdf,
        &names,
        b"/EmbeddedFiles",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let names = resolved_key(
        pdf,
        &embedded_files,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;

    // qpdf collects into `std::map<std::string, shared_ptr<Buffer>>`
    // (`test_driver.cc:1270`), so the print loop below iterates in ascending
    // filename byte order regardless of array position, and a repeated
    // filename keeps only the *last* array entry that produced it (plain
    // `operator[]` assignment, `test_driver.cc:1282`) -- both of which
    // `BTreeMap::insert` reproduces directly.
    let mut attachments: BTreeMap<Vec<u8>, Rc<Vec<u8>>> = BTreeMap::new();
    for item in names.as_array().unwrap_or_default() {
        let item = resolved_terminal(pdf, &item, filename, diagnostics_written, stdout, stderr)?;
        let Some(ef_f) = matching_filespec_ef_f_stream(
            pdf,
            &item,
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?
        else {
            continue;
        };
        let filename_value = resolved_key(
            pdf,
            &item,
            b"/F",
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?
        .as_string()
        .unwrap_or_default();
        let data = ef_f.get_stream_data(DecodeLevel::Generalized)?;
        attachments.insert(filename_value, data);
    }

    for (attachment_name, data) in attachments {
        write_bytes(stdout, &attachment_name)?;
        stdout.write_all(b":\n")?;
        // qpdf's `data.at(i) < 0` reads a `char` as signed (the typical
        // platform default this crate targets), so combined with
        // `data.at(i) > 126` the condition is exactly "byte outside
        // 0..=126" -- i.e. byte >= 127 read as `u8` (`test_driver.cc:1290-1295`).
        let is_binary = data.iter().any(|&byte| byte > 126);
        if is_binary {
            let mut summary = Vec::new();
            for &byte in data.iter().take(20) {
                if (32..=126).contains(&byte) {
                    summary.push(byte);
                } else {
                    summary.push(b'.');
                }
            }
            summary.extend_from_slice(format!(" ({} bytes)", data.len()).as_bytes());
            write_bytes(stdout, &summary)?;
        } else {
            write_bytes(stdout, &data)?;
        }
        stdout.write_all(b"--END--\n")?;
    }
    Ok(())
}

pub(crate) fn run_test_36<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root = root_handle(pdf, filename, diagnostics_written, stdout, stderr)?;
    let names = resolved_key(
        pdf,
        &root,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let embedded_files = resolved_key(
        pdf,
        &names,
        b"/EmbeddedFiles",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    let names = resolved_key(
        pdf,
        &embedded_files,
        b"/Names",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;

    for item in names.as_array().unwrap_or_default() {
        let item = resolved_terminal(pdf, &item, filename, diagnostics_written, stdout, stderr)?;
        let Some(ef_f) = matching_filespec_ef_f_stream(
            pdf,
            &item,
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?
        else {
            continue;
        };
        let filename_handle = resolved_key(
            pdf,
            &item,
            b"/F",
            filename,
            diagnostics_written,
            stdout,
            stderr,
        )?;
        if filename_handle.as_string().as_deref() != Some(b"attachment1.txt".as_slice()) {
            continue;
        }
        let attachment_name = filename_handle.as_string().unwrap_or_default();

        // `stream.pipeStreamData(&p2, 0, qpdf_dl_none)` pipes the raw,
        // undecoded stream bytes through a bare `Pl_Flate` inflate stage
        // (`test_driver.cc:1329-1331`), bypassing the object's own
        // `/DecodeParms` (in particular any predictor) entirely.
        // `get_raw_stream_data` is `QPDF_Stream::getRawStreamData`, the same
        // undecoded source `pipeStreamData(dl_none)` reads
        // (`ObjectHandle::get_raw_stream_data`'s own doc), and a synthetic
        // one-filter `/FlateDecode` dictionary with no `/DecodeParms`
        // reproduces a bare raw-inflate stage with no predictor applied.
        let raw = ef_f.get_raw_stream_data()?;
        let synthetic_filter = ObjectHandle::dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::name(b"FlateDecode".to_vec()),
        )]);
        let data = flpdf::filters::decode_stream_data(&synthetic_filter, &raw)?;

        let dict_handle = ef_f
            .as_stream_dict()
            .expect("matching_filespec_ef_f_stream already confirmed a stream value");
        write_bytes(stdout, &dict_handle.unparse())?;
        write_bytes(stdout, &attachment_name)?;
        stdout.write_all(b":\n")?;
        write_bytes(stdout, &data)?;
        stdout.write_all(b"--END--\n")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_37 (`test_driver.cc:1340-1348`, `ParserCallbacks` at `:98-134`)
// ---------------------------------------------------------------------------

struct ContentParserCallbacks<'a> {
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
    /// qpdf's own content-stream input-source name, `"page object N G"`
    /// (`QPDFObjectHandle::parsePageContents`, `libqpdf/QPDFObjectHandle.cc:1740-1743`).
    description: String,
}

impl<'a> flpdf::ObjectHandleParserCallbacks for ContentParserCallbacks<'a> {
    fn content_size(&mut self, size: usize) -> flpdf::Result<()> {
        writeln!(self.stdout, "content size: {size}")?;
        Ok(())
    }

    // Content-stream parser-recovery diagnostics never reach
    // `Pdf::repair_diagnostics()` -- `ContentHandleParser`'s own
    // `take_diagnostics()` is a channel local to this callback boundary
    // (`content_stream.rs`'s `parse_content_stream_handles`), unlike every
    // other diagnostic source this driver drains via
    // `emit_new_diagnostics`. Silently accepting the trait's no-op default
    // here would drop warnings qpdf's own driver prints. qpdf's real
    // dispatch is two different `QPDFExc` shapes reaching the same
    // `context->warn(...)` sink:
    // - `QPDFParser`'s own internal recovery warnings use
    //   `object_description = "content"` (the literal string
    //   `QPDFObjectHandle.cc:1812` passes as the `QPDFParser` constructor's
    //   second argument, stored as `QPDFParser::object_description`,
    //   `libqpdf/qpdf/QPDFParser.hh:14-27`, and read back by
    //   `QPDFParser::warn(offset, msg)`, `libqpdf/QPDFParser.cc:510-513`).
    // - The one inline-image-EOF diagnostic is instead constructed directly
    //   in `QPDFObjectHandle::parseContentStream_data` with the literal
    //   object description `"stream data"` (`libqpdf/QPDFObjectHandle.cc:1831-1838`).
    // This trait's `handle_diagnostic(offset, message)` carries no flag
    // distinguishing the two call sites, so the fixed, only-ever-used-there
    // message text from the second case (`content_stream.rs`'s own two
    // `"EOF found while reading inline image"` literals, matching
    // `libqpdf/QPDFObjectHandle.cc:1838,1848` verbatim) is used as the
    // discriminator instead. UNVERIFIED against real qpdf output -- flagged
    // in this file's own top-level caveats.
    fn handle_diagnostic(&mut self, offset: usize, message: &str) -> flpdf::Result<()> {
        let object = if message == "EOF found while reading inline image" {
            "stream data"
        } else {
            "content"
        };
        // `QPDFExc::createWhat` (`libqpdf/QPDFExc.cc:18-51`), with
        // `filename` = `self.description` (qpdf's `input->getName()`, *not*
        // this driver's own PDF file path -- content-stream diagnostics
        // have no document-file offset at all, only a position within the
        // concatenated content-stream buffer), printed by qpdf's default
        // warning callback as `"WARNING: " + exc.what()`.
        let mut what = self.description.clone();
        if !what.is_empty() {
            what.push_str(" (");
        }
        what.push_str(object);
        if offset > 0 {
            what.push_str(&format!(", offset {offset}"));
        }
        if !self.description.is_empty() {
            what.push(')');
        }
        what.push_str(": ");
        what.push_str(message);
        self.stdout.flush()?;
        self.stderr.write_all(b"WARNING: ")?;
        self.stderr.write_all(what.as_bytes())?;
        self.stderr.write_all(b"\n")?;
        Ok(())
    }

    fn handle_object(
        &mut self,
        object: ObjectHandle,
        offset: usize,
        length: usize,
    ) -> flpdf::Result<flpdf::ParseControl> {
        if object.as_name().as_deref() == Some(b"Abort".as_slice()) {
            writeln!(self.stdout, "test suite: terminating parsing")?;
            // `terminateParsing()` throws immediately
            // (`test_driver.cc:116-119`), so the type/offset/length line
            // below never executes for the `/Abort` token itself.
            return Ok(flpdf::ParseControl::Stop);
        }
        let type_name = object.type_name()?;
        write!(
            self.stdout,
            "{}, offset={offset}, length={length}: ",
            type_name
        )?;
        if object.type_code()? == 12 {
            // ot_inlineimage
            let value = object.as_inline_image().unwrap_or_default();
            writeln!(self.stdout, "{}", hex_encode(&value))?;
        } else {
            write_bytes(self.stdout, &object.unparse())?;
            writeln!(self.stdout)?;
        }
        Ok(flpdf::ParseControl::Continue)
    }

    fn handle_eof(&mut self) -> flpdf::Result<()> {
        writeln!(self.stdout, "-EOF-")?;
        Ok(())
    }
}

pub(crate) fn run_test_37<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for page_ref in page_refs {
        let page = pdf.get_object_handle(page_ref);
        let description = format!("page object {} {}", page_ref.number, page_ref.generation);
        let mut callbacks = ContentParserCallbacks {
            stdout,
            stderr,
            description,
        };
        page.parse_page_contents(&mut callbacks)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_38 (`test_driver.cc:1350-1358`) -- designed for override-compressed-object.pdf
// ---------------------------------------------------------------------------

pub(crate) fn run_test_38<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root = root_handle(pdf, filename, diagnostics_written, stdout, stderr)?;
    let qtest = resolved_key(
        pdf,
        &root,
        b"/QTest",
        filename,
        diagnostics_written,
        stdout,
        stderr,
    )?;
    for item in qtest.as_array().unwrap_or_default() {
        let resolved =
            resolved_terminal(pdf, &item, filename, diagnostics_written, stdout, stderr)?;
        write_bytes(stdout, &resolved.unparse_resolved())?;
        writeln!(stdout)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_39 (`test_driver.cc:1360-1375`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_39<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for (index, page_ref) in page_refs.into_iter().enumerate() {
        writeln!(stdout, "page {}", index + 1)?;

        // `QPDFPageObjectHelper::getImages` walks this page's *own*
        // `/Resources /XObject` (inherited up `/Parent` when the leaf page
        // has none) without mutating the page tree
        // (`libqpdf/QPDFPageObjectHelper.cc:318-383`).
        // `PageObjectHelper::get_resources` is the same non-mutating inherited
        // handle walk, so it is used here rather
        // than `push_inherited_attributes_to_pages`, which *would* write
        // `/Resources` down onto every page -- a side effect test_39's own
        // qpdf source never has.
        let resources = PageObjectHelper::new(page_ref, pdf).get_resources(false)?;
        let resources = pdf.resolve_to_terminal(&resources)?;
        if resources.is_null() {
            continue;
        }
        let xobject = pdf.resolve_to_terminal(&resources.get_key(b"/XObject"))?;
        let Some(xobject) = xobject.as_dictionary() else {
            continue;
        };

        // `std::map<std::string, QPDFObjectHandle>` (`QPDFPageObjectHelper::getImages`,
        // `:375-384`) iterates in ascending XObject-name order; the canonical
        // handle dictionary snapshot is BTreeMap-ordered and does the same.
        for (_key, value) in xobject {
            let value = pdf.resolve_to_terminal(&value)?;
            let Some(object_ref) = value.object_ref() else {
                continue;
            };
            let handle = pdf.get_object_handle(object_ref);
            if !handle.is_image(true)? {
                continue;
            }
            let dict = handle
                .as_stream_dict()
                .expect("is_image(true) already confirmed a stream value");
            let filter = pdf
                .resolve_to_terminal(&dict.get_key(b"/Filter"))?
                .unparse_resolved();
            let color_space = pdf
                .resolve_to_terminal(&dict.get_key(b"/ColorSpace"))?
                .unparse_resolved();
            write!(stdout, "filter: ")?;
            write_bytes(stdout, &filter)?;
            write!(stdout, ", color space: ")?;
            write_bytes(stdout, &color_space)?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_40 (`test_driver.cc:1377-1389`)
// ---------------------------------------------------------------------------

pub(crate) fn run_test_40<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    assert!(
        arg2.is_some(),
        "test 40 requires arg2 (qpdf's own `assert(arg2 != nullptr)`, test_driver.cc:1384)"
    );
    let arg2 = arg2.expect("checked above");
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file(arg2)?;
    writer.set_pclm(true);
    writer.set_static_id(true);
    writer.write()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// test_41 (`test_driver.cc:1391-1404`, `TokenFilter` at `:136-157`)
// ---------------------------------------------------------------------------

/// Ports qpdf's own `TokenFilter` test class (`test_driver.cc:136-157`).
/// This crate's `flpdf::Token` (`libqpdf/QPDFTokenizer.hh`'s `Token`
/// equivalent) has no public constructor -- only its accompanying parser can
/// build one -- so the two synthetic replacement tokens qpdf's version
/// constructs (`Token(tt_string, "Salad")`, `Token(tt_name, "/bye")`) are
/// written here as their already-known canonical raw spellings instead of
/// being built through the (private) `Token` constructor. Both literals are
/// plain ASCII with no characters this crate's own canonical-raw rules
/// (`tokenizer.rs`'s `canonical_string_raw`/`canonical_name_raw`, which this
/// comment's byte sequences were checked against) would ever escape, so
/// `(Salad)` and `/bye` are exactly the bytes `writeToken` would emit for
/// them -- this is not a fabricated value, only a hand-computed one for a
/// fixed, already-known literal.
struct PotatoSaladTokenFilter;

impl TokenFilter for PotatoSaladTokenFilter {
    fn handle_token(
        &mut self,
        token: &flpdf::ContentToken,
        output: &mut TokenFilterOutput<'_>,
    ) -> flpdf::PipelineResult<()> {
        if token.token_type == flpdf::ContentTokenType::String && token.value == b"Potato" {
            output.write(b"(Salad)")?;
        } else {
            output.write_token(token)?;
        }
        Ok(())
    }

    fn handle_eof(&mut self, output: &mut TokenFilterOutput<'_>) -> flpdf::PipelineResult<()> {
        output.write(b"/bye")?;
        output.write(b"\n")?;
        Ok(())
    }
}

pub(crate) fn run_test_41<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
    for page_ref in page_refs {
        let page = pdf.get_object_handle(page_ref);
        let filter: Rc<RefCell<dyn TokenFilter>> = Rc::new(RefCell::new(PotatoSaladTokenFilter));
        page.add_content_token_filter(filter)?;
    }
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_qdf_mode(true);
    writer.set_static_id(true);
    writer.write()?;
    Ok(())
}
