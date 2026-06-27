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
//! * object numbers form the complete set `1..N` (no gaps, no duplicates) —
//!   [`fix_qdf`] rejects any other numbering. (qpdf's `fix-qdf` is stricter
//!   still: it also requires ascending *file* order. [`fix_qdf`] tolerates a
//!   complete-but-unordered numbering because flpdf's own QDF writer can emit a
//!   reused indirect `/Length` holder out of file order, and `fix_qdf` must be
//!   able to repair its own output.);
//! * stream lengths are stored as an *indirect* reference `/Length M G R`, with
//!   the length itself living in a standalone `M G obj` whose body is a single
//!   integer (qpdf canonical QDF never inlines a direct `/Length <n>` for an
//!   actual stream — the oracle does not fix a direct length either);
//! * a single classic `xref` table with one `0 N` subsection, followed by a
//!   `trailer` dictionary, `startxref`, and `%%EOF`.
//!
//! Object streams (`/Type /ObjStm`) are not handled: QDF mode disables them,
//! so they should never appear. If one is present
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
//! 3. **trailer `/Size`** — object count + 1 (equivalently the highest object
//!    number + 1, since numbering is contiguous `1..N`).
//! 4. **`startxref`** — the byte offset of the `xref` keyword that begins the
//!    regenerated table.
//!
//! Running [`fix_qdf`] on an already-valid QDF file is a no-op, and the
//! function is idempotent: `fix_qdf(fix_qdf(x)) == fix_qdf(x)`.

use crate::parser::{is_delimiter, is_ws};
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
    buf[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| from + i)
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

/// Locate the **last** line-anchored `kw` keyword in `input`.
///
/// The genuine cross-reference table always follows every object, at the tail
/// of the file. A QDF stream body (streams are decompressed in QDF) can itself
/// contain a line that begins with `xref`, so scanning from the top would match
/// stream content. Taking the last line-anchored match selects the real table.
fn rfind_line_keyword(input: &[u8], kw: &[u8]) -> Option<usize> {
    let mut found = None;
    let mut from = 0;
    while let Some(pos) = find_line_keyword_from(input, kw, from) {
        found = Some(pos);
        from = pos + 1;
    }
    found
}

/// Find the PDF name token `name` (e.g. `b"/Length"`, `b"/Size"`,
/// `b"/ObjStm"`) inside `hay`, in object syntax only.
///
/// Walks the bytes skipping literal strings `(...)` (balanced parens, `\`
/// escapes), hex strings `<...>` (while treating `<<`/`>>` as dictionary
/// delimiters, not strings), and `%` comments, so a copy of `name` appearing
/// inside a string/comment is ignored. `name` matches only when the byte
/// immediately after it is a PDF whitespace/delimiter (ISO 32000-1 §7.2) or
/// end-of-slice, so `/Length1`, `/SizeExtra`, etc. are not mistaken for
/// `/Length` / `/Size`. Returns the start offset of the match.
fn find_name_token(hay: &[u8], name: &[u8]) -> Option<usize> {
    find_name_token_from(hay, name, 0)
}

/// Like [`find_name_token`] but starts scanning at `from`. `from` must be a
/// position in normal object context (not inside a string/hex/comment) — all
/// internal callers pass either `0` or a position just past a previously
/// matched name token, which satisfies this.
fn find_name_token_from(hay: &[u8], name: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < hay.len() {
        match hay[i] {
            // `%` comment runs to end of line (PDF §7.2.4).
            b'%' => {
                while i < hay.len() && hay[i] != b'\n' && hay[i] != b'\r' {
                    i += 1;
                }
            }
            // `<<` / `>>` are dict delimiters, not hex strings.
            b'<' if hay.get(i + 1) == Some(&b'<') => i += 2,
            b'>' if hay.get(i + 1) == Some(&b'>') => i += 2,
            // Hex string `<...>` — skip to the closing `>`.
            b'<' => {
                i += 1;
                while i < hay.len() && hay[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            // Literal string `(...)` — balanced parens, `\` escapes.
            b'(' => {
                i += 1;
                let mut depth = 1usize;
                while i < hay.len() && depth > 0 {
                    match hay[i] {
                        b'\\' => i += 1, // skip escaped byte
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            // Candidate name token in normal object context.
            b'/' if hay[i..].starts_with(name) => {
                let after = hay.get(i + name.len());
                if after.is_none_or(|&b| is_ws(b) || is_delimiter(b)) {
                    return Some(i);
                }
                i += 1; // longer name (`/Length1` etc.) — keep scanning.
            }
            _ => i += 1,
        }
    }
    None
}

/// Scan a stream dictionary slice for `/Length M G R` (indirect) or
/// `/Length <int>` (direct). Returns `Indirect(M)` or `Direct`.
enum LengthKind {
    /// Indirect `/Length M 0 R` — canonical QDF only ever uses generation 0.
    Indirect(u32),
    /// Indirect `/Length M G R` with `G != 0` — not canonical QDF; qdf_fix
    /// keys holders by object number only, so a non-zero generation cannot be
    /// validated/rewritten safely. Treated as an explicit error rather than
    /// silently rewriting the wrong-generation object.
    IndirectUnsupportedGeneration,
    Direct,
    None,
}

fn classify_length(dict: &[u8]) -> LengthKind {
    // Find the PDF *name token* `/Length` in object syntax (skipping strings,
    // hex strings, and comments; requiring a trailing token boundary so
    // `/Length1` etc. do not match).
    let needle = b"/Length";
    let Some(p) = find_name_token(dict, needle) else {
        return LengthKind::None;
    };
    let rest = &dict[p + needle.len()..];
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
        if let (Ok(gen), "R") = (g.parse::<u32>(), r) {
            return if gen == 0 {
                LengthKind::Indirect(first.parse().unwrap())
            } else {
                LengthKind::IndirectUnsupportedGeneration
            };
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
    // An object stream is `<< ... /Type /ObjStm ... >>`. Only reject when a
    // `/Type` *name token* has the *value* name token `/ObjStm` — not merely
    // any `/ObjStm` (it could be an unrelated name value like
    // `/SomeKey /ObjStm`, and copies in strings/comments are skipped anyway).
    let mut from = 0;
    while let Some(tp) = find_name_token_from(body, b"/Type", from) {
        let mut j = tp + b"/Type".len();
        // Skip PDF whitespace AND `%...EOL` comments between the key and its
        // value (comments are token separators too — `/Type %c\n /ObjStm`).
        loop {
            match body.get(j) {
                Some(&b) if is_ws(b) => j += 1,
                Some(&b'%') => {
                    while body.get(j).is_some_and(|&c| c != b'\n' && c != b'\r') {
                        j += 1;
                    }
                }
                _ => break,
            }
        }
        if body[j..].starts_with(b"/ObjStm")
            && body
                .get(j + b"/ObjStm".len())
                .is_none_or(|&b| is_ws(b) || is_delimiter(b))
        {
            return true;
        }
        from = tp + b"/Type".len();
    }
    false
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
///   table, malformed trailer, an indirect `/Length` whose holder object is
///   missing, or object numbers that do not form the complete set `1..N`).
pub fn fix_qdf(input: &[u8]) -> Result<Vec<u8>> {
    // ---- 1. Locate the xref / trailer / startxref region. ---------------
    // We rebuild everything from the real `xref` table (the LAST line-anchored
    // `xref`, since a decompressed stream body may contain an `xref` line)
    // through end of file.
    let xref_pos = rfind_line_keyword(input, b"xref")
        .ok_or_else(|| Error::parse(0, "fix_qdf: no classic `xref` table found"))?;

    let body_region = &input[..xref_pos];

    // ---- 2. Parse all `N G obj` spans in the body region. ---------------
    let mut objects: Vec<ObjectSpan> = Vec::new();
    let mut cursor = 0usize;
    while let Some((num, gen, line_start, kw_end)) = find_next_obj(body_region, cursor) {
        // Determine whether this object contains a stream BEFORE searching for
        // `endobj`. A decompressed QDF stream body may itself contain a line
        // that starts with `endobj`, which would truncate the object span if we
        // searched for `endobj` naively. The real `endobj` always follows the
        // `endstream` keyword, so for stream objects we anchor the search there.
        let mut stream_info: Option<(usize, usize, usize)> = None; // (stream_kw, content_start, endstream_kw)
                                                                   // A real stream's `stream` keyword follows this object's dictionary
                                                                   // close `>>`. `find_matching_dict_close` skips literal strings, hex
                                                                   // strings, and `%` comments, so a `stream`/`endstream` byte sequence
                                                                   // inside a NON-stream object's string value (which lives *inside*
                                                                   // `<<...>>`, before the close) is never mistaken for a real stream.
                                                                   // For dict-less objects (e.g. bare-integer length holders) the first
                                                                   // `<<` belongs to a later object; the `stream_is_ours` endobj-
                                                                   // precedence check below then correctly rejects it.
        let after_dict = find_subslice(&body_region[kw_end..], b"<<")
            .map(|o| kw_end + o)
            .and_then(|d| find_matching_dict_close(body_region, d))
            .map(|c| c + 2);
        if let Some(stream_kw) =
            after_dict.and_then(|sf| find_line_keyword_from(body_region, b"stream", sf))
        {
            // Only treat it as this object's stream if there is no `endobj`
            // before the `stream` keyword (otherwise the stream belongs to a
            // later object).
            let first_endobj = find_line_keyword_from(body_region, b"endobj", kw_end);
            let stream_is_ours = match first_endobj {
                None => true,
                Some(eob) => stream_kw < eob,
            };
            if stream_is_ours {
                // Compute content_start: just past the `stream` EOL.
                let mut content_start = stream_kw + b"stream".len();
                if body_region.get(content_start) == Some(&b'\r') {
                    content_start += 1;
                }
                if body_region.get(content_start) == Some(&b'\n') {
                    content_start += 1;
                }
                // Search for `endstream` starting from content_start.
                if let Some(endstream_kw) =
                    find_line_keyword_from(body_region, b"endstream", content_start)
                {
                    stream_info = Some((stream_kw, content_start, endstream_kw));
                }
            }
        }

        // `endobj` search begins AFTER `endstream` when a stream is present, so
        // that a line-anchored `endobj` inside the stream body is not mistaken
        // for the object terminator.
        let endobj_search_from = match stream_info {
            Some((_, _, endstream_kw)) => endstream_kw + b"endstream".len(),
            None => kw_end,
        };
        let endobj = find_line_keyword_from(body_region, b"endobj", endobj_search_from)
            .ok_or_else(|| Error::parse(line_start, "fix_qdf: object without matching `endobj`"))?;
        let end = endobj + b"endobj".len();
        let body = &body_region[kw_end..endobj];

        if detect_objstm(body) {
            return Err(Error::Unsupported(
                "fix_qdf: object streams (/Type /ObjStm) are not supported in QDF input".into(),
            ));
        }

        // Determine stream length and indirect /Length holder using the already-
        // computed stream span (avoids rescanning body_region).
        let mut stream_len = None;
        let mut length_holder = None;
        if let Some((stream_kw_abs, content_start_abs, endstream_kw_abs)) = stream_info {
            // Stream length: verbatim bytes between content_start and endstream.
            stream_len = Some(endstream_kw_abs - content_start_abs);
            // Dictionary is everything between kw_end and the `stream` keyword
            // (relative to kw_end, which is where `body` starts).
            let dict = &body_region[kw_end..stream_kw_abs];
            match classify_length(dict) {
                LengthKind::Indirect(m) => length_holder = Some(m),
                LengthKind::IndirectUnsupportedGeneration => {
                    return Err(Error::parse(
                        line_start,
                        "fix_qdf: stream /Length holder with non-zero generation \
                         is not supported (canonical QDF uses generation 0)",
                    ));
                }
                LengthKind::Direct | LengthKind::None => {
                    // Canonical qpdf QDF always uses an indirect length for
                    // real streams; the oracle does not rewrite a direct
                    // one. Leave it untouched (verbatim preservation).
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

    // The object numbers must form the COMPLETE set `1..N` — every number in
    // `1..=objects.len()` present exactly once, no gaps, no duplicates, nothing
    // out of range. This is the security-relevant invariant: it bounds the
    // regenerated xref to the object count, so a sparse or huge object number
    // can no longer drive `/Size` and the table length far beyond the actual
    // object count (the previous `0..max_num+1` dense form let a tiny input with
    // one huge number force a multi-gigabyte table and overflow `max_num + 1`).
    //
    // We deliberately do NOT additionally require qpdf's *file order* here.
    // qpdf's fix-qdf is stricter (QdfFixer::checkObjId fatals unless objects
    // appear in ascending file order), but flpdf's own QDF writer may emit a
    // reused indirect `/Length` holder out of ascending file order (holders are
    // collected and emitted after the main objects), producing a complete but
    // unordered numbering. fix_qdf must still repair its own writer's output, so
    // order-tolerance is retained and the xref below is emitted in ascending
    // object-number order regardless of input order. Strict qpdf file-order
    // parity is deferred until the writer emits holders in qpdf's position.
    let n = objects.len();
    let mut seen = vec![false; n];
    for obj in &objects {
        let num = obj.num as usize;
        // Short-circuit keeps `seen[num - 1]` in bounds: it is only indexed when
        // `1 <= num <= n`. `replace` returns the prior flag — `true` means this
        // number already appeared (a duplicate).
        if num == 0 || num > n || std::mem::replace(&mut seen[num - 1], true) {
            return Err(Error::parse(
                obj.obj_line_start,
                "fix_qdf: object numbers are not a complete 1..N set \
                 (gap, duplicate, or out-of-range object number)",
            ));
        }
    }

    // ---- 3. Compute the new length-holder integer bodies. ---------------
    // Validate every indirect `/Length M G R` holder (flpdf-9hc.25):
    //   * the holder object `M` must actually exist in the parsed set —
    //     otherwise the "repaired" file still carries a dangling indirect
    //     length and is invalid for downstream readers; and
    //   * a holder reused by two streams with *conflicting* lengths is an
    //     explicit error rather than silent last-writer-wins (which would
    //     leave the earlier stream's /Length wrong).
    // A canonical QDF indirect /Length is always `M 0 R` (generation 0;
    // non-zero generations are rejected above). The holder must therefore be
    // an object whose number is M AND whose generation is 0 — matching on the
    // number alone would wrongly accept/rewrite an `M G` object with G != 0.
    let gen0_object_numbers: std::collections::HashSet<u32> = objects
        .iter()
        .filter(|o| o.gen == 0)
        .map(|o| o.num)
        .collect();
    let mut new_len_body: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for obj in &objects {
        if let (Some(len), Some(holder)) = (obj.stream_len, obj.length_holder) {
            if !gen0_object_numbers.contains(&holder) {
                return Err(Error::parse(
                    obj.obj_line_start,
                    "fix_qdf: stream's indirect /Length holder object (M 0) is missing",
                ));
            }
            if let Some(&prev) = new_len_body.get(&holder) {
                if prev != len {
                    return Err(Error::parse(
                        obj.obj_line_start,
                        "fix_qdf: indirect /Length holder reused with conflicting lengths",
                    ));
                }
            }
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

        // Only a generation-0 object can be the holder a canonical `M 0 R`
        // /Length points at — never rewrite an `M G` object with G != 0.
        if let Some(&new_len) = new_len_body.get(&obj.num).filter(|_| obj.gen == 0) {
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
    // qpdf's fix-qdf (QdfFixer::st_at_xref) writes a `0 <1+n>` subsection header,
    // the free-list head, then one in-use entry per object. `/Size` is exactly
    // `objects.len() + 1` (qpdf's `1 + xref.size()`); sizing from the object
    // count — not the maximum object number — is what bounds the table and
    // avoids any `max_num + 1` overflow.
    //
    // Entries are emitted in ascending object-number order. A `BTreeMap` keyed
    // by object number makes that independent of the order the objects appeared
    // in the file: numbering was validated as the complete set `1..N`, but it
    // may be unordered (flpdf's writer can emit a reused /Length holder out of
    // file order), and an xref subsection must list its entries by number.
    let startxref_value = out.len();
    let size = objects.len() + 1;
    let by_num: std::collections::BTreeMap<u32, (u32, usize)> = new_offsets
        .iter()
        .map(|&(num, gen, off)| (num, (gen, off)))
        .collect();

    out.extend_from_slice(b"xref\n");
    out.extend_from_slice(format!("0 {size}\n").as_bytes());
    // Object 0 is the free-list head, exactly as qpdf fix-qdf emits it.
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (gen, off) in by_num.values() {
        out.extend_from_slice(format!("{off:010} {gen:05} n \n").as_bytes());
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

/// Find a keyword that begins a line (preceded by start-of-buffer or `\n`),
/// at or after `from`.
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
    let lead = body
        .iter()
        .take_while(|&&b| b.is_ascii_whitespace())
        .count();
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
    while i < input.len() {
        match input[i] {
            // `%` comment runs to end of line.
            b'%' => {
                while i < input.len() && input[i] != b'\n' && input[i] != b'\r' {
                    i += 1;
                }
            }
            // `<<` / `>>` are dict delimiters (checked before single `<`/`>`).
            b'<' if input.get(i + 1) == Some(&b'<') => {
                depth += 1;
                i += 2;
            }
            b'>' if input.get(i + 1) == Some(&b'>') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 2;
            }
            // Hex string `<...>` — skip to the closing `>`.
            b'<' => {
                i += 1;
                while i < input.len() && input[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            // Literal string `(...)` — balanced parens, `\` escapes.
            b'(' => {
                i += 1;
                let mut sdepth = 1usize;
                while i < input.len() && sdepth > 0 {
                    match input[i] {
                        b'\\' => i += 1, // skip escaped byte
                        b'(' => sdepth += 1,
                        b')' => sdepth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Rewrite the `/Size <n>` entry inside a trailer dictionary slice.
fn rewrite_size(trailer: &[u8], size: usize) -> Vec<u8> {
    // `/Size` as a real name token only — skip strings/hex/comments and
    // reject `/SizeExtra` etc. via the trailing token-boundary check.
    let Some(p) = find_name_token(trailer, b"/Size") else {
        return trailer.to_vec();
    };
    let mut out = Vec::with_capacity(trailer.len() + 4);
    out.extend_from_slice(&trailer[..p + b"/Size".len()]);
    let rest = &trailer[p + b"/Size".len()..];
    // Skip whitespace, then the old integer.
    let ws = rest
        .iter()
        .take_while(|&&b| b.is_ascii_whitespace())
        .count();
    let digits = rest[ws..].iter().take_while(|b| b.is_ascii_digit()).count();
    out.extend_from_slice(&rest[..ws]);
    out.extend_from_slice(size.to_string().as_bytes());
    out.extend_from_slice(&rest[ws + digits..]);
    out
}
