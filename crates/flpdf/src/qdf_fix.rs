//! `fix_qdf`: the flpdf equivalent of qpdf's `fix-qdf` tool.
//!
//! After a human edits a QDF-form PDF (the flat, normalized layout produced by
//! [`crate::write_qdf`] / `qpdf --qdf`), the cross-reference offsets, stream
//! `/Length` values, the trailer `/Size`, and `startxref` are all stale. This
//! module recomputes exactly those four regions from the current bytes while
//! preserving every other byte verbatim — object bodies, comments, formatting,
//! and any whitespace the human introduced are kept exactly as-is.
//!
//! ## Scope and format expectations
//!
//! [`fix_qdf`] operates purely on bytes; it does **not** route through the full
//! [`crate::Pdf`] parser/serializer (that would reformat the file and defeat the
//! "must not change human-edited content" guarantee). It targets the canonical
//! QDF structure that `qpdf --qdf` produces and that qpdf's own `fix-qdf`
//! accepts:
//!
//! * objects are written as `N G obj` at the start of a line, optionally
//!   preceded by a `%% Original object ID: N G` comment line (the offset always
//!   points at the `N G obj` line, **not** the comment — verified against the
//!   `fix-qdf` oracle);
//! * stream lengths are stored as an *indirect* reference `/Length M G R`, with
//!   the length itself living in a standalone `M G obj` whose body is a single
//!   integer (qpdf canonical QDF never inlines a direct `/Length <n>` for an
//!   actual stream — the oracle does not fix a direct length either);
//! * a single classic `xref` table with one `0 N` subsection, followed by a
//!   `trailer` dictionary, `startxref`, and `%%EOF`.
//!
//! Object streams (`/Type /ObjStm`) are not handled: QDF mode disables them
//! (epic layer 6.2), so they should never appear. If one is present
//! [`fix_qdf`] returns [`crate::Error::Unsupported`].
//!
//! ## The four regenerated regions
//!
//! 1. **Stream `/Length`** — for every stream object the length is the exact
//!    number of bytes between the end of the line containing the `stream`
//!    keyword and the `endstream` keyword: counting starts at the first byte
//!    after the `stream` keyword's end-of-line marker (`\r\n`, `\n`, or `\r`)
//!    and ends at (but excludes) the `endstream` keyword. No EOL normalization
//!    is performed — the count is verbatim. The recomputed value is written
//!    into the indirect length object's body as a plain decimal integer (no
//!    zero padding).
//! 2. **xref offsets** — each in-use object's 10-digit offset is the byte
//!    offset of the start of its `N G obj` line in the *rewritten* output.
//! 3. **trailer `/Size`** — highest object number + 1.
//! 4. **`startxref`** — the byte offset of the `xref` keyword that begins the
//!    regenerated table.
//!
//! Running [`fix_qdf`] on an already-valid QDF file is a no-op, and the
//! function is idempotent: `fix_qdf(fix_qdf(x)) == fix_qdf(x)`.

use crate::{Error, Result};

/// One parsed `N G obj ... endobj` body in the input.
#[derive(Debug, Clone)]
struct ObjectSpan {
    num: u32,
    gen: u32,
    /// Byte offset (in the *input*) of the start of the `N G obj` line.
    obj_line_start: usize,
    /// Byte offset (in the *input*) one past the `endobj` keyword's line
    /// (start of the next byte region, used as this object's end bound).
    end: usize,
    /// If this object directly contains a stream, the verbatim recomputed
    /// `/Length` value (byte count between the `stream` EOL and `endstream`).
    stream_len: Option<usize>,
    /// If this object's stream dict uses an indirect `/Length M G R`, the
    /// object number `M` that holds the length integer.
    length_holder: Option<u32>,
}

/// Find the next line that begins exactly with `N G obj` at `from`, scanning
/// line by line. Returns `(num, gen, line_start, content_after_obj_kw)`.
fn find_next_obj(input: &[u8], from: usize) -> Option<(u32, u32, usize, usize)> {
    let mut line_start = from;
    while line_start < input.len() {
        let line_end = memchr_nl(input, line_start).unwrap_or(input.len());
        let line = &input[line_start..line_end];
        if let Some((num, gen, kw_end)) = parse_obj_header(line) {
            return Some((num, gen, line_start, line_start + kw_end));
        }
        line_start = line_end + 1;
        if line_end >= input.len() {
            break;
        }
    }
    None
}

/// Index of the next `\n` at or after `from`.
fn memchr_nl(buf: &[u8], from: usize) -> Option<usize> {
    buf[from..].iter().position(|&b| b == b'\n').map(|i| from + i)
}

/// Parse a line that should be `N G obj` (with optional trailing content after
/// the `obj` keyword, e.g. nothing in canonical QDF). Returns
/// `(num, gen, byte index just past "obj")` on success.
fn parse_obj_header(line: &[u8]) -> Option<(u32, u32, usize)> {
    // Trim a trailing '\r' (CRLF inputs).
    let line = if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    };
    let s = std::str::from_utf8(line).ok()?;
    let mut it = s.split_ascii_whitespace();
    let num: u32 = it.next()?.parse().ok()?;
    let gen: u32 = it.next()?.parse().ok()?;
    if it.next()? != "obj" {
        return None;
    }
    // canonical QDF puts nothing else on the line; reject if there is.
    if it.next().is_some() {
        return None;
    }
    let kw_end = s.rfind("obj")? + 3;
    Some((num, gen, kw_end))
}

/// Locate `needle` as a standalone keyword (preceded by start-of-buffer or an
/// ASCII delimiter/whitespace) at or after `from`.
fn find_keyword(input: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + needle.len() <= input.len() {
        if &input[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Compute the verbatim stream length for an object whose body starts a stream.
///
/// `body` is the bytes from just after the `N G obj` line to the object's
/// `endobj`. Returns `Some(len)` if a `stream` keyword is found.
fn compute_stream_len(body: &[u8]) -> Option<usize> {
    // Find the `stream` keyword. It appears on its own line after the dict.
    let kw = find_keyword(body, b"stream", 0)?;
    // Skip the keyword and its end-of-line marker (CRLF, LF, or lone CR), as
    // per the PDF stream convention. Content begins immediately after.
    let mut content_start = kw + b"stream".len();
    if body.get(content_start) == Some(&b'\r') {
        content_start += 1;
    }
    if body.get(content_start) == Some(&b'\n') {
        content_start += 1;
    }
    // `endstream` is only the terminator when it starts a line — matching
    // qpdf's `fix-qdf` convention. This prevents an `endstream` byte sequence
    // appearing inside binary/text stream content from truncating the count.
    let end = find_line_keyword_from(body, b"endstream", content_start)?;
    Some(end - content_start)
}

/// Scan a stream dictionary slice for `/Length M G R` (indirect) or
/// `/Length <int>` (direct). Returns `Indirect(M)` or `Direct`.
enum LengthKind {
    Indirect(u32),
    Direct,
    None,
}

fn classify_length(dict: &[u8]) -> LengthKind {
    let Some(p) = find_subslice(dict, b"/Length") else {
        return LengthKind::None;
    };
    let rest = &dict[p + b"/Length".len()..];
    let s = match std::str::from_utf8(rest) {
        Ok(s) => s,
        Err(_) => return LengthKind::None,
    };
    let mut it = s.split_ascii_whitespace();
    let Some(first) = it.next() else {
        return LengthKind::None;
    };
    if first.parse::<u32>().is_err() {
        return LengthKind::None;
    }
    // Indirect form: `<int> <int> R`
    let second = it.next();
    let third = it.next();
    if let (Some(g), Some(r)) = (second, third) {
        if g.parse::<u32>().is_ok() && r == "R" {
            return LengthKind::Indirect(first.parse().unwrap());
        }
    }
    LengthKind::Direct
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The recomputed value to be written into a length-holder object.
fn detect_objstm(body: &[u8]) -> bool {
    find_subslice(body, b"/ObjStm").is_some()
}

/// Read and recompute a hand-edited QDF file.
///
/// See the module docs for the exact rules. Returns the corrected bytes.
///
/// # Errors
///
/// * [`Error::Unsupported`] if an object stream (`/Type /ObjStm`) is present
///   (QDF mode disables object streams, so this should not occur in practice).
/// * [`Error::Parse`] if the input does not look like a QDF file (no `xref`
///   table, malformed trailer, or an indirect `/Length` whose holder object is
///   missing).
pub fn fix_qdf(input: &[u8]) -> Result<Vec<u8>> {
    // ---- 1. Locate the xref / trailer / startxref region. ---------------
    // We rebuild everything from the first `xref` keyword that starts a line
    // through end of file.
    let xref_pos = find_line_keyword(input, b"xref")
        .ok_or_else(|| Error::parse(0, "fix_qdf: no classic `xref` table found"))?;

    let body_region = &input[..xref_pos];

    // ---- 2. Parse all `N G obj` spans in the body region. ---------------
    let mut objects: Vec<ObjectSpan> = Vec::new();
    let mut cursor = 0usize;
    while let Some((num, gen, line_start, kw_end)) = find_next_obj(body_region, cursor) {
        // Object body runs until the matching `endobj` keyword.
        let endobj = find_line_keyword_from(body_region, b"endobj", kw_end).ok_or_else(|| {
            Error::parse(line_start, "fix_qdf: object without matching `endobj`")
        })?;
        let end = endobj + b"endobj".len();
        let body = &body_region[kw_end..endobj];

        if detect_objstm(body) {
            return Err(Error::Unsupported(
                "fix_qdf: object streams (/Type /ObjStm) are not supported in QDF input".into(),
            ));
        }

        // Determine the stream-dict slice (between `<<` ... `>>`) if any, then
        // whether this object has a stream and how its /Length is stored.
        let mut stream_len = None;
        let mut length_holder = None;
        if let Some(stream_kw) = find_keyword(body, b"stream", 0) {
            // `endstream` must follow; if not this `stream` is incidental text.
            if find_keyword(body, b"endstream", stream_kw).is_some() {
                stream_len = compute_stream_len(body);
                // The dictionary is everything before the `stream` keyword.
                let dict = &body[..stream_kw];
                match classify_length(dict) {
                    LengthKind::Indirect(m) => length_holder = Some(m),
                    LengthKind::Direct | LengthKind::None => {
                        // Canonical qpdf QDF always uses an indirect length for
                        // real streams; the oracle does not rewrite a direct
                        // one. Leave it untouched (verbatim preservation).
                    }
                }
            }
        }

        objects.push(ObjectSpan {
            num,
            gen,
            obj_line_start: line_start,
            end,
            stream_len,
            length_holder,
        });
        cursor = end;
    }

    if objects.is_empty() {
        return Err(Error::parse(0, "fix_qdf: no objects found before xref"));
    }

    // ---- 3. Compute the new length-holder integer bodies. ---------------
    // Map object number -> its index in `objects`.
    let mut new_len_body: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for obj in &objects {
        if let (Some(len), Some(holder)) = (obj.stream_len, obj.length_holder) {
            new_len_body.insert(holder, len);
        }
    }

    // ---- 4. Emit the rewritten body, substituting length-holder bodies and
    //         recording each object's new offset. -------------------------
    let mut out: Vec<u8> = Vec::with_capacity(input.len() + 16);
    // Everything before the first object is the header (%PDF / binary marker /
    // %QDF / blank lines) — copied verbatim.
    let first_obj_start = objects[0].obj_line_start;
    out.extend_from_slice(&body_region[..first_obj_start]);

    // New byte offset of each object number (by index in `objects`).
    let mut new_offsets: Vec<(u32, u32, usize)> = Vec::with_capacity(objects.len());

    for (i, obj) in objects.iter().enumerate() {
        // Copy any inter-object bytes (comments like `%% Original object ID`,
        // blank lines) that sit between the previous object end and this
        // object's line start — verbatim. For the first object this range is
        // empty (header already copied).
        if i > 0 {
            let prev_end = objects[i - 1].end;
            out.extend_from_slice(&body_region[prev_end..obj.obj_line_start]);
        }

        // This object's offset = current output length (start of `N G obj`).
        new_offsets.push((obj.num, obj.gen, out.len()));

        if let Some(&new_len) = new_len_body.get(&obj.num) {
            // Rewrite this length-holder object: keep the `N G obj` line and
            // `endobj`, replace the integer body with the recomputed value.
            rewrite_length_holder(&mut out, &body_region[obj.obj_line_start..obj.end], new_len)?;
        } else {
            // Copy the object verbatim.
            out.extend_from_slice(&body_region[obj.obj_line_start..obj.end]);
        }
    }

    // Copy bytes between the last object's end and the `xref` keyword
    // (blank lines etc.) verbatim.
    let last_end = objects.last().unwrap().end;
    out.extend_from_slice(&body_region[last_end..xref_pos]);

    // ---- 5. Emit the regenerated xref table. ----------------------------
    let startxref_value = out.len();
    let max_num = objects.iter().map(|o| o.num).max().unwrap();
    let size = max_num + 1;

    // Build offset lookup: object number -> (gen, offset). Object 0 is the
    // free-list head.
    let mut offset_by_num: std::collections::HashMap<u32, (u32, usize)> =
        std::collections::HashMap::new();
    for &(num, gen, off) in &new_offsets {
        offset_by_num.insert(num, (gen, off));
    }

    out.extend_from_slice(b"xref\n");
    out.extend_from_slice(format!("0 {size}\n").as_bytes());
    for n in 0..size {
        if n == 0 {
            // Free-list head, exactly as qpdf fix-qdf emits it.
            out.extend_from_slice(b"0000000000 65535 f \n");
        } else if let Some(&(gen, off)) = offset_by_num.get(&n) {
            out.extend_from_slice(format!("{off:010} {gen:05} n \n").as_bytes());
        } else {
            // A gap (object number not present): emit a free entry. Canonical
            // QDF is contiguous so this is defensive only.
            out.extend_from_slice(b"0000000000 00000 f \n");
        }
    }

    // ---- 6. Emit trailer / startxref / %%EOF. ---------------------------
    // Reuse the original trailer dictionary verbatim except for /Size, which
    // we rewrite. Locate the original trailer text after the old xref region.
    let trailer_kw = find_subslice(&input[xref_pos..], b"trailer")
        .map(|p| xref_pos + p)
        .ok_or_else(|| Error::parse(xref_pos, "fix_qdf: no `trailer` keyword"))?;
    // Trailer dictionary spans the first `<<` to its matching `>>`.
    let dict_open = find_subslice(&input[trailer_kw..], b"<<")
        .map(|p| trailer_kw + p)
        .ok_or_else(|| Error::parse(trailer_kw, "fix_qdf: trailer has no dictionary"))?;
    let dict_close = find_matching_dict_close(input, dict_open)
        .ok_or_else(|| Error::parse(dict_open, "fix_qdf: unterminated trailer dictionary"))?;

    // Copy `trailer` ... up to and including the dict, with /Size rewritten.
    let trailer_prefix = &input[xref_pos..trailer_kw]; // usually empty
    // (the xref body we already emitted ourselves; trailer_prefix is bytes
    // between old xref keyword and `trailer`, which we *replaced*, so ignore).
    let _ = trailer_prefix;
    let trailer_dict = &input[trailer_kw..dict_close + 2];
    let rewritten_trailer = rewrite_size(trailer_dict, size);
    out.extend_from_slice(&rewritten_trailer);

    // Copy whatever sits between `>>` and `startxref` verbatim (newline,
    // optional `/Prev` lines do not occur in QDF; just whitespace).
    let after_dict = dict_close + 2;
    let startxref_kw = find_subslice(&input[after_dict..], b"startxref")
        .map(|p| after_dict + p)
        .ok_or_else(|| Error::parse(after_dict, "fix_qdf: no `startxref` keyword"))?;
    out.extend_from_slice(&input[after_dict..startxref_kw]);

    // `startxref` then its value line, recomputed.
    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(format!("{startxref_value}\n").as_bytes());

    // Finally the `%%EOF` (and any trailing bytes) copied verbatim.
    let eof = find_subslice(&input[startxref_kw..], b"%%EOF")
        .map(|p| startxref_kw + p)
        .ok_or_else(|| Error::parse(startxref_kw, "fix_qdf: no `%%EOF` marker"))?;
    out.extend_from_slice(&input[eof..]);

    Ok(out)
}

/// Find a keyword that begins a line (preceded by start-of-buffer or `\n`).
fn find_line_keyword(input: &[u8], kw: &[u8]) -> Option<usize> {
    find_line_keyword_from(input, kw, 0)
}

fn find_line_keyword_from(input: &[u8], kw: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + kw.len() <= input.len() {
        if &input[i..i + kw.len()] == kw {
            let at_line_start = i == 0 || input[i - 1] == b'\n' || input[i - 1] == b'\r';
            // The keyword must be followed by EOL/EOF/whitespace so we don't
            // match `xref` inside `startxref` or `endstream` inside text.
            let after_ok = match input.get(i + kw.len()) {
                None => true,
                Some(&c) => c == b'\n' || c == b'\r' || c == b' ' || c == b'\t',
            };
            if at_line_start && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Given the verbatim bytes of a length-holder object (`N G obj\n<int>\nendobj`
/// possibly with different whitespace), emit it with the integer replaced by
/// `new_len`, preserving the `obj`/`endobj` lines and surrounding whitespace.
fn rewrite_length_holder(out: &mut Vec<u8>, obj_bytes: &[u8], new_len: usize) -> Result<()> {
    // Find end of the `N G obj` header line.
    let nl = obj_bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| Error::parse(0, "fix_qdf: malformed length object header"))?;
    // Header (including its newline) copied verbatim.
    out.extend_from_slice(&obj_bytes[..=nl]);

    // The body is everything up to the `endobj` keyword. Preserve leading and
    // trailing whitespace around the integer so the file shape is kept.
    let endobj_rel = find_line_keyword_from(obj_bytes, b"endobj", nl + 1)
        .ok_or_else(|| Error::parse(0, "fix_qdf: length object missing endobj"))?;
    let body = &obj_bytes[nl + 1..endobj_rel];

    // Split body into leading whitespace, the integer token, trailing bytes.
    let lead = body.iter().take_while(|&&b| b.is_ascii_whitespace()).count();
    let after_int = body[lead..]
        .iter()
        .position(|&b| !b.is_ascii_digit())
        .map(|p| lead + p)
        .unwrap_or(body.len());
    // Sanity: the token between lead..after_int must be all digits.
    if after_int == lead || !body[lead..after_int].iter().all(|b| b.is_ascii_digit()) {
        return Err(Error::parse(
            0,
            "fix_qdf: length-holder body is not a plain integer",
        ));
    }
    out.extend_from_slice(&body[..lead]);
    out.extend_from_slice(new_len.to_string().as_bytes());
    out.extend_from_slice(&body[after_int..]);

    // Emit `endobj` and the rest of the object verbatim.
    out.extend_from_slice(&obj_bytes[endobj_rel..]);
    Ok(())
}

/// Find the `>>` that closes the dictionary opened by `<<` at `open`,
/// accounting for nesting.
fn find_matching_dict_close(input: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i + 1 < input.len() {
        if &input[i..i + 2] == b"<<" {
            depth += 1;
            i += 2;
            continue;
        }
        if &input[i..i + 2] == b">>" {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

/// Rewrite the `/Size <n>` entry inside a trailer dictionary slice.
fn rewrite_size(trailer: &[u8], size: u32) -> Vec<u8> {
    let Some(p) = find_subslice(trailer, b"/Size") else {
        return trailer.to_vec();
    };
    let mut out = Vec::with_capacity(trailer.len() + 4);
    out.extend_from_slice(&trailer[..p + b"/Size".len()]);
    let rest = &trailer[p + b"/Size".len()..];
    // Skip whitespace, then the old integer.
    let ws = rest.iter().take_while(|&&b| b.is_ascii_whitespace()).count();
    let digits = rest[ws..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    out.extend_from_slice(&rest[..ws]);
    out.extend_from_slice(size.to_string().as_bytes());
    out.extend_from_slice(&rest[ws + digits..]);
    out
}
