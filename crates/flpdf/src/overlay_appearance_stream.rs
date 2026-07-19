//! Port of qpdf's `AcroForm::adjustAppearanceStream` and its
//! `ResourceReplacer` / `ResourceFinder` token filter
//! (`libqpdf/QPDFAcroFormDocumentHelper.cc:628-849`, `libqpdf/ResourceFinder.cc`),
//! called from [`crate::overlay_annotations`]'s `transform_annot_ap_streams`
//! once per (already per-placement-dup'd) `/AP` appearance stream whenever a
//! placement's [`crate::overlay_annotations::DrMap`] is non-empty.
//!
//! An appearance stream copied from another document may reference resource
//! names (a font, an `ExtGState`, ...) through its own `/Resources`
//! dictionary that collided with the destination `/AcroForm/DR` during the
//! merge and were renamed there (`DrMap`, populated by
//! `merge_resources_shallow`). Left alone, the stream's content would still
//! say e.g. `/F1 18 Tf` while the destination's merged `/DR/Font` no longer
//! has an `F1` entry — only `F1_1`. [`adjust_appearance_stream`] privatizes
//! the stream's `/Resources`, renames the colliding keys there, and rewrites
//! the matching name tokens in the stream's own content so both stay
//! internally consistent.
//!
//! [`resource_replacer`] is the content-rewriting half in isolation (no
//! `Pdf` access, pure byte transform), split out so it — and the
//! operator→resource-type table it uses — can be unit tested directly
//! against small content-stream fragments.

use std::io::{Read, Seek};

use crate::overlay_annotations::DrMap;
use crate::parser::{is_delimiter, is_ws, Parser};
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};

/// Resource category for a content-stream operator that consumes a resource
/// name as one of its operands, mirroring qpdf's `op_to_rtype` table in
/// `ResourceFinder::handleObject` (`libqpdf/ResourceFinder.cc:6-16`).
/// Returns `None` for every operator not in that table (including operators
/// that take no resource-name operand at all).
fn resource_type_for_operator(op: &[u8]) -> Option<&'static [u8]> {
    Some(match op {
        b"CS" | b"cs" => b"ColorSpace",
        b"gs" => b"ExtGState",
        b"Tf" => b"Font",
        b"SCN" | b"scn" => b"Pattern",
        b"BDC" | b"DP" => b"Properties",
        b"sh" => b"Shading",
        b"Do" => b"XObject",
        _ => return None,
    })
}

/// Rewrite resource-name tokens in appearance-stream `content` according to
/// `dr_map`, matching qpdf's `ResourceReplacer::handleToken`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:675-687`).
///
/// For every operator recognised by [`resource_type_for_operator`], the
/// single most-recently-seen `Name` token is looked up — qpdf's
/// `ResourceFinder::last_name` (`libqpdf/ResourceFinder.cc:5-27`) is
/// overwritten by *every* `Name` object it sees, regardless of how many
/// other operands intervene, and is consulted (not cleared) whenever an
/// operator follows. This one-slot tracking is why `BDC`/`DP`'s *second*
/// name operand (the properties-dict name, e.g. `/P1` in `/Span /P1 BDC`) is
/// what gets matched: the first name (the tag, `/Span`) is overwritten by
/// the second before the operator is reached.
///
/// A match requires BOTH:
/// - `dr_map` records a rename for that name under the operator's resource
///   category (populated by `merge_resources_shallow` when the source
///   `/DR`'s entry collided with an existing destination entry under the
///   same name), AND
/// - `resources[category]` — the appearance stream's own, **pre-rename**
///   `/Resources` sub-dictionary, snapshotted by the caller before
///   [`adjust_appearance_stream`]'s in-place rename — still carries that
///   name as a key. Real qpdf's `ResourceReplacer` has no such guard (its
///   `to_replace` map is built solely from `dr_map` crossed with the
///   content scan); this is an flpdf-local defensive check mirroring the
///   one `crate::overlay_annotations::adjust_default_appearance` applies to
///   `/DA` strings, and is a no-op superset check in practice: every name
///   `merge_resources_shallow` records a rename for was, by construction,
///   present in the source `/DR` that this stream's `/Resources` was copied
///   from.
///
/// When matched, the name token's exact byte span is replaced with the
/// renamed name (escaped via [`crate::object::write_name_escaped`], the
/// same escaping the `/DA` rewriter uses). Every other byte — whitespace,
/// comments, unrelated operands, unrelated operators — is copied through
/// unchanged, so the result is byte-identical to `content` except at each
/// rewritten name's exact span.
///
/// A malformed operand is copied through verbatim, one byte at a time,
/// resuming the scan just past it (the same tolerant recovery
/// `adjust_default_appearance` uses) rather than aborting the whole
/// rewrite.
///
/// Inline images (`BI`...`ID`...`EI`) ARE recognized specially, matching
/// qpdf's tokenizer, which emits the whole `ID`-to-`EI` span as a single
/// opaque `tt_inline_image` token that a `TokenFilter` (such as
/// `ResourceReplacer`) never re-lexes (`libqpdf/QPDFTokenizer.cc`'s
/// `expectInlineImage`/`findEI`, called from
/// `QPDFObjectHandle::parseContentStream_data` on every `ID` operator):
/// once the `ID` operator is seen, everything up to the next
/// delimiter-bounded `EI` is copied through byte-for-byte without being fed
/// to the operand/operator lexer, so binary image data that happens to
/// contain a byte sequence resembling a resource-name operand (e.g. `/F1 18
/// Tf`) is never mistaken for one. This implements qpdf's primary
/// delimiter-bounded-`EI` search only, not its secondary ten-token lookahead
/// heuristic for disambiguating an `EI` byte sequence that occurs *inside*
/// the image data itself — no shipped fixture needs that refinement, and
/// the per-byte malformed-operand recovery above still guarantees
/// termination even if a real file needed it.
///
/// Returns `content.to_vec()` verbatim, without scanning, when `dr_map` is
/// empty (the common case: no placement recorded a rename on this dest
/// page).
pub(crate) fn resource_replacer(content: &[u8], dr_map: &DrMap, resources: &Dictionary) -> Vec<u8> {
    if dr_map.is_empty() {
        return content.to_vec();
    }

    let mut out: Vec<u8> = Vec::with_capacity(content.len());
    // Byte span of the most recently seen name token WITHIN `out` (not
    // `content` — needed so `Vec::splice` can replace it in place) plus its
    // decoded value. Overwritten by every subsequent name token; consumed
    // (reset to `None`) only when a table operator actually applies it, so
    // a later stray operator cannot re-splice an already-rewritten span.
    let mut last_name: Option<(usize, usize, Vec<u8>)> = None;
    let mut pos = 0usize;
    while pos < content.len() {
        let byte = content[pos];
        if is_ws(byte) {
            let start = pos;
            while pos < content.len() && is_ws(content[pos]) {
                pos += 1;
            }
            out.extend_from_slice(&content[start..pos]);
            continue;
        }
        if byte == b'%' {
            // `%` comment: copied verbatim to end of line.
            let start = pos;
            while pos < content.len() && !matches!(content[pos], b'\n' | b'\r') {
                pos += 1;
            }
            out.extend_from_slice(&content[start..pos]);
            continue;
        }
        if byte == b'/'
            || byte == b'('
            || byte == b'<'
            || byte == b'['
            || matches!(byte, b'+' | b'-' | b'.' | b'0'..=b'9')
        {
            // Operand: delegate to the shared object lexer (numbers,
            // strings, names, arrays, dictionaries), matching how
            // `crate::content_stream` and `adjust_default_appearance` both
            // reuse it, so name/string escaping is identical everywhere.
            let mut parser = Parser::new_no_reference(&content[pos..]);
            match parser.parse_one_object() {
                Ok(obj) => {
                    let end = pos + parser.position();
                    let out_start = out.len();
                    out.extend_from_slice(&content[pos..end]);
                    if let Object::Name(name) = obj {
                        last_name = Some((out_start, out.len(), name));
                    }
                    pos = end;
                }
                Err(_) => {
                    // Malformed operand: copy one byte verbatim and resume
                    // (tolerant scanning — see doc comment above).
                    out.push(byte);
                    pos += 1;
                }
            }
            continue;
        }
        // Operator keyword: bytes up to the next whitespace/delimiter.
        let start = pos;
        while pos < content.len() && !is_ws(content[pos]) && !is_delimiter(content[pos]) {
            pos += 1;
        }
        if pos == start {
            // Stray delimiter that did not start a recognised operand (e.g.
            // an unmatched `)`); copy the single byte verbatim and resume.
            out.push(content[pos]);
            pos += 1;
            continue;
        }
        let op = &content[start..pos];
        out.extend_from_slice(op);
        if op == b"ID" {
            // Inline image data: per the doc comment above, everything from
            // here to the next delimiter-bounded `EI` is opaque and must be
            // copied through verbatim, never fed to the name/operator
            // lexer. `last_name` is deliberately left untouched — an inline
            // image is a single token to qpdf's tokenizer too, so it can
            // neither set nor consume it.
            //
            // The single mandatory separator byte right after `ID` (ISO
            // 32000-2 8.9.7.2) is itself part of the opaque span, but is
            // copied separately here so the search below starts exactly at
            // the first real data byte.
            if pos < content.len() {
                out.push(content[pos]);
                pos += 1;
            }
            let data_start = pos;
            let mut ei_pos = content.len();
            let mut i = data_start;
            while i + 1 < content.len() {
                if content[i] == b'E'
                    && content[i + 1] == b'I'
                    && (i == data_start || is_ws(content[i - 1]))
                    && (i + 2 >= content.len()
                        || is_ws(content[i + 2])
                        || is_delimiter(content[i + 2]))
                {
                    ei_pos = i;
                    break;
                }
                i += 1;
            }
            // `ei_pos` falls back to `content.len()` (copy everything to
            // EOF as opaque data) when no delimiter-bounded `EI` is found —
            // a truncated/malformed stream, handled the same tolerant way
            // as every other recovery path in this scanner: never panic or
            // hang.
            out.extend_from_slice(&content[data_start..ei_pos]);
            pos = ei_pos;
            continue;
        }
        if let Some(rtype) = resource_type_for_operator(op) {
            if let Some((out_start, out_end, name)) = last_name.take() {
                let renamed = dr_map.category(rtype).and_then(|m| m.get(name.as_slice()));
                if let Some(new_name) = renamed {
                    let present = resources
                        .get(rtype)
                        .and_then(Object::as_dict)
                        .is_some_and(|d| d.get(name.as_slice()).is_some());
                    if present {
                        let mut replacement = Vec::with_capacity(new_name.len() + 1);
                        replacement.push(b'/');
                        crate::object::write_name_escaped(&mut replacement, new_name);
                        out.splice(out_start..out_end, replacement);
                    }
                }
            } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
        }
    }
    out
}

/// Privatize and rewrite an appearance stream's `/Resources` dictionary and
/// content so a resource name that collided during the destination
/// `/AcroForm/DR` merge ([`DrMap`], populated by
/// `crate::overlay_annotations::merge_resources_shallow`) resolves to the
/// renamed destination name, matching qpdf's `AcroForm::adjustAppearanceStream`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:752-849`). Called once per
/// (already per-placement-dup'd) `/AP` stream from
/// `crate::overlay_annotations::transform_annot_ap_streams`, after the
/// stream's `/Matrix` has already been concatenated with the placement's
/// `cm`.
///
/// Does nothing when `dr_map` is empty, or when the stream at
/// `ap_stream_ref` has no `/Resources` entry — together with the caller's
/// own `!dr_map.is_empty()` check, this reproduces the two-part gate qpdf
/// applies at the `transformAnnotations` call site (`libqpdf/QPDFAcroFormDocumentHelper.cc:1160-1161`:
/// `if (!dr_map.empty() && resources)`) before invoking this function at
/// all.
///
/// # Algorithm
///
/// 1. Resolve `/Resources` to an owned [`Dictionary`], noting whether it was
///    reached through an indirect reference. Every stream that reaches this
///    point gets its OWN private copy — never shared with another
///    placement's dup of the same stream, or with the source
///    `/AcroForm/DR` object the copy started out pointing at — matching
///    qpdf's unconditional `resources.shallowCopy()`.
/// 2. For every resource-type category [`DrMap::categories`] recorded a
///    rename for: ensure that category's sub-dictionary exists in the
///    private `/Resources` copy (inserting an empty one if absent — mirrors
///    merging in qpdf's per-category empty `merge_with` dict, whose only
///    effect at this stage is to force the sub-dictionary to exist and be
///    unshared), then rename every `old_key` present in that sub-dictionary
///    to `dr_map`'s `new_key`, keeping the same value.
/// 3. Drop any sub-dictionary left empty by step 2 (qpdf: "remove empty
///    subdictionaries").
/// 4. Rewrite the stream's decoded content via [`resource_replacer`], using
///    the **pre-rename** `/Resources` snapshot taken in step 1 — the
///    membership guard must see the original names, since step 2 already
///    renamed (removed) them from the copy being written back.
///
/// The rare double-conflict case qpdf handles by re-merging with a
/// `merge_with` side table and growing its (per-call, by-value) copy of
/// `dr_map` — reached only when the private `/Resources` copy already has
/// an entry under the *renamed* name that differs from what step 2 is
/// moving into it — is not implemented: no shipped qpdf-oracle golden
/// exercises it, and `dr_map` here is an immutable map shared across every
/// placement on this destination page, so growing it locally would leak a
/// stream-specific rename across placements. See
/// `libqpdf/QPDFAcroFormDocumentHelper.cc:806-816`.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`], or from decoding/re-encoding
/// the stream's content (an unsupported or malformed `/Filter` chain).
pub(crate) fn adjust_appearance_stream<R: Read + Seek>(
    dest: &mut Pdf<R>,
    ap_stream_ref: ObjectRef,
    dr_map: &DrMap,
) -> Result<()> {
    if dr_map.is_empty() {
        return Ok(());
    }
    let Object::Stream(mut stream) = dest.resolve(ap_stream_ref)? else {
        return Ok(()); // cov:ignore: defensive early return — caller only ever passes a stream ref
    };
    let Some(resources_val) = stream.dict.get("Resources").cloned() else {
        return Ok(());
    };
    let (mut resources, was_indirect) = match resources_val {
        Object::Dictionary(d) => (d, false),
        Object::Reference(r) => match dest.resolve(r)?.into_dict() {
            Some(d) => (d, true),
            None => return Ok(()), // cov:ignore: /Resources ref does not resolve to a dict — malformed input
        },
        _ => return Ok(()), // cov:ignore: /Resources neither dict nor reference — malformed input
    };

    // Snapshot BEFORE any rename: `resource_replacer`'s membership guard
    // must see the ORIGINAL names — the rename loop below removes them from
    // `resources` (the copy that gets written back), so consulting the
    // post-rename copy would make every match fail.
    let pre_rename_resources = resources.clone();

    for category in dr_map.categories() {
        let Some(renames) = dr_map.category(category) else {
            continue; // cov:ignore: category() looked up from categories()'s own keys — never None
        };
        let existing = resources.get(category).cloned();
        let mut subdict = match existing {
            Some(Object::Dictionary(d)) => d,
            Some(Object::Reference(r)) => dest.resolve(r)?.into_dict().unwrap_or_default(), // cov:ignore: indirect resource-type sub-dict inside an AP stream's own /Resources — no shipped fixture supplies this shape.
            _ => Dictionary::new(),
        };
        for (old_key, new_key) in renames {
            if let Some(val) = subdict.remove(old_key) {
                subdict.insert(new_key, val);
            }
        }
        resources.insert(category, Object::Dictionary(subdict));
    }

    // Remove empty sub-dictionaries (qpdf: "Remove empty subdictionaries").
    let empty_categories: Vec<Vec<u8>> = resources
        .iter()
        .filter_map(|(key, value)| match value {
            Object::Dictionary(d) if d.iter().next().is_none() => Some(key.to_vec()),
            _ => None,
        })
        .collect();
    for key in empty_categories {
        resources.remove(&key);
    }

    let decoded = crate::filters::decode_stream_data(&stream.dict, &stream.data)?;
    let new_decoded = resource_replacer(&decoded, dr_map, &pre_rename_resources);
    stream.data = crate::filters::encode_stream_data(&stream.dict, &new_decoded)?;

    if was_indirect {
        // A fresh indirect object, never the original ref: the original
        // still identifies the (possibly shared-across-placements) source
        // `/DR` copy this stream's `/Resources` started out pointing at.
        // Overwriting it in place would corrupt every other consumer of
        // that ref; the writer's existing reachability pass drops the now-
        // unreferenced original once nothing points at it any more.
        let new_ref = allocate_next_ref(dest)?;
        dest.set_object(new_ref, Object::Dictionary(resources));
        stream.dict.insert("Resources", Object::Reference(new_ref));
    } else {
        stream
            .dict
            .insert("Resources", Object::Dictionary(resources));
    }
    dest.set_object(ap_stream_ref, Object::Stream(stream));
    Ok(())
}

/// Allocate a fresh indirect object ref (`max(numbers) + 1`, gen 0).
/// Duplicate of the crate-local helper in `overlay_annotations.rs` /
/// `overlay.rs` / `page_form_xobject.rs` — kept module-local so this file
/// has no dependency on `overlay_annotations.rs`'s private surface.
fn allocate_next_ref<R: Read + Seek>(dest: &Pdf<R>) -> Result<ObjectRef> {
    let n = dest
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| crate::Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectRef;
    use std::io::Cursor;

    /// Build a `DrMap` with a single category's rename table. `DrMap`'s
    /// `by_name` field is private to `overlay_annotations`, so tests drive
    /// it through the `#[cfg(test)]`-only `DrMap::for_test` constructor
    /// added there for exactly this purpose, rather than round-tripping
    /// through a full `merge_resources_shallow` call over a real `Pdf`.
    fn dr_map_with(category: &[u8], old: &[u8], new: &[u8]) -> DrMap {
        crate::overlay_annotations::DrMap::for_test(category, old, new)
    }

    fn dict_with_subdict(category: &[u8], keys: &[&[u8]]) -> Dictionary {
        let mut sub = Dictionary::new();
        for k in keys {
            sub.insert(*k, Object::Integer(1));
        }
        let mut d = Dictionary::new();
        d.insert(category, Object::Dictionary(sub));
        d
    }

    // ---- resource_replacer -------------------------------------------------

    #[test]
    fn resource_replacer_empty_dr_map_is_identity() {
        let dr_map = DrMap::new();
        let resources = Dictionary::new();
        let content = b"/F1 18 Tf (hi) Tj";
        assert_eq!(resource_replacer(content, &dr_map, &resources), content);
    }

    #[test]
    fn resource_replacer_rewrites_tf_font_name() {
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content = b"/F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map, &resources),
            b"/F1_1 18 Tf"
        );
    }

    #[test]
    fn resource_replacer_name_not_in_dr_map_is_verbatim() {
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F2"]);
        let content = b"/F2 18 Tf";
        assert_eq!(resource_replacer(content, &dr_map, &resources), content);
    }

    #[test]
    fn resource_replacer_name_absent_from_resources_is_verbatim() {
        // dr_map has a rename, but this stream's own /Resources never had
        // the old name — the membership guard must block the rewrite.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = Dictionary::new();
        let content = b"/F1 18 Tf";
        assert_eq!(resource_replacer(content, &dr_map, &resources), content);
    }

    #[test]
    fn resource_replacer_name_inside_string_literal_is_verbatim() {
        // `(F1)` is a STRING, not a Name — `Tj` is not in the operator
        // table either, so nothing here can match regardless.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content = b"(F1) Tj";
        assert_eq!(resource_replacer(content, &dr_map, &resources), content);
    }

    #[test]
    fn resource_replacer_preserves_whitespace_and_comments() {
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content: &[u8] = b"  % a comment\n /F1   18  Tf  ";
        assert_eq!(
            resource_replacer(content, &dr_map, &resources),
            b"  % a comment\n /F1_1   18  Tf  ".to_vec()
        );
    }

    #[test]
    fn resource_replacer_recovers_from_malformed_token_midstream() {
        // A stray, unmatched `)` is not a valid operand start; the scanner
        // must copy it through verbatim and keep going rather than
        // dropping the rest of the stream (mirrors
        // `crate::overlay_annotations::adjust_default_appearance`'s own
        // malformed-token recovery test).
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content: &[u8] = b"/F1 18 Tf ) /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map, &resources),
            b"/F1_1 18 Tf ) /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_recovers_from_operand_parse_error() {
        // An unterminated string literal (opens `(` but never closes) IS a
        // recognised operand start and reaches the shared object lexer,
        // which returns `Err` on EOF — distinct from the stray-delimiter
        // path above, which never reaches the lexer at all. The scanner
        // must copy only the single `(` byte and resume rather than losing
        // the rest of the stream, so the `/F1` rename after it still
        // applies (mirrors
        // `crate::overlay_annotations::adjust_default_appearance`'s own
        // operand-parse-error recovery test).
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content: &[u8] = b"(bad /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map, &resources),
            b"(bad /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_do_xobject() {
        let dr_map = dr_map_with(b"XObject", b"Fx1", b"Fx1_1");
        let resources = dict_with_subdict(b"XObject", &[b"Fx1"]);
        assert_eq!(
            resource_replacer(b"q /Fx1 Do Q", &dr_map, &resources),
            b"q /Fx1_1 Do Q".to_vec()
        );
    }

    #[test]
    fn resource_replacer_gs_extgstate() {
        let dr_map = dr_map_with(b"ExtGState", b"GS1", b"GS1_1");
        let resources = dict_with_subdict(b"ExtGState", &[b"GS1"]);
        assert_eq!(
            resource_replacer(b"/GS1 gs", &dr_map, &resources),
            b"/GS1_1 gs".to_vec()
        );
    }

    #[test]
    fn resource_replacer_sh_shading() {
        let dr_map = dr_map_with(b"Shading", b"Sh1", b"Sh1_1");
        let resources = dict_with_subdict(b"Shading", &[b"Sh1"]);
        assert_eq!(
            resource_replacer(b"/Sh1 sh", &dr_map, &resources),
            b"/Sh1_1 sh".to_vec()
        );
    }

    #[test]
    fn resource_replacer_cs_and_lowercase_cs_colorspace() {
        let dr_map = dr_map_with(b"ColorSpace", b"CS1", b"CS1_1");
        let resources = dict_with_subdict(b"ColorSpace", &[b"CS1"]);
        assert_eq!(
            resource_replacer(b"/CS1 CS", &dr_map, &resources),
            b"/CS1_1 CS".to_vec()
        );
        assert_eq!(
            resource_replacer(b"/CS1 cs", &dr_map, &resources),
            b"/CS1_1 cs".to_vec()
        );
    }

    #[test]
    fn resource_replacer_scn_and_lowercase_scn_pattern() {
        let dr_map = dr_map_with(b"Pattern", b"P1", b"P1_1");
        let resources = dict_with_subdict(b"Pattern", &[b"P1"]);
        assert_eq!(
            resource_replacer(b"1 0 0 /P1 SCN", &dr_map, &resources),
            b"1 0 0 /P1_1 SCN".to_vec()
        );
        assert_eq!(
            resource_replacer(b"1 0 0 /P1 scn", &dr_map, &resources),
            b"1 0 0 /P1_1 scn".to_vec()
        );
    }

    #[test]
    fn resource_replacer_bdc_and_dp_properties_use_second_name() {
        // `last_name` is overwritten by every Name token seen, regardless
        // of position — for BDC's two name operands (tag, then properties
        // name) the SECOND one (`/P1`) is what's in scope when the
        // operator is reached, so only it is eligible for renaming; the
        // first (`/Span`) is never looked up at all.
        let dr_map = dr_map_with(b"Properties", b"P1", b"P1_1");
        let resources = dict_with_subdict(b"Properties", &[b"P1"]);
        assert_eq!(
            resource_replacer(b"/Span /P1 BDC", &dr_map, &resources),
            b"/Span /P1_1 BDC".to_vec()
        );
        assert_eq!(
            resource_replacer(b"/Span /P1 DP", &dr_map, &resources),
            b"/Span /P1_1 DP".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_data_is_opaque() {
        // A byte sequence that looks exactly like a renameable operand
        // (`/F1 18 Tf`) sits INSIDE the inline image's binary data, between
        // `ID` and `EI`. It must be copied through verbatim — only the real
        // content tokens before `BI` and after `EI` are eligible for
        // renaming.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content: &[u8] =
            b"/F1 18 Tf BI /W 1 /H 1 /BPC 8 /CS /G ID \x01/F1 18 Tf\x02 EI /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map, &resources),
            b"/F1_1 18 Tf BI /W 1 /H 1 /BPC 8 /CS /G ID \x01/F1 18 Tf\x02 EI /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_without_ei_copies_to_eof() {
        // A truncated/malformed inline image with no delimiter-bounded `EI`
        // at all: the fallback must treat everything from `ID` to the end
        // of `content` as opaque data rather than losing bytes or hanging,
        // and must NOT rewrite the `/F1` pattern embedded in it.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let resources = dict_with_subdict(b"Font", &[b"F1"]);
        let content: &[u8] = b"BI /W 1 ID \x01/F1 18 Tf\x02";
        assert_eq!(resource_replacer(content, &dr_map, &resources), content);
    }

    // ---- adjust_appearance_stream -------------------------------------------

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(
            format!("xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n").as_bytes(),
        );
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn open_minimal() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(minimal_pdf_bytes())).expect("minimal pdf should parse")
    }

    fn set_dict<R: Read + Seek>(pdf: &mut Pdf<R>, n: u32, entries: &[(&str, Object)]) -> ObjectRef {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert(*k, v.clone());
        }
        let r = ObjectRef::new(n, 0);
        pdf.set_object(r, Object::Dictionary(d));
        r
    }

    fn set_stream<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        n: u32,
        entries: &[(&str, Object)],
        data: &[u8],
    ) -> ObjectRef {
        let mut d = Dictionary::new();
        for (k, v) in entries {
            d.insert(*k, v.clone());
        }
        let r = ObjectRef::new(n, 0);
        pdf.set_object(r, Object::Stream(crate::Stream::new(d, data.to_vec())));
        r
    }

    #[test]
    fn adjust_appearance_stream_empty_dr_map_is_noop() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(font_ref));
                    d
                }),
            )],
        );
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Reference(resources_ref))],
            b"/F1 18 Tf",
        );

        adjust_appearance_stream(&mut pdf, ap_ref, &DrMap::new()).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1 18 Tf");
        assert_eq!(
            stream.dict.get("Resources"),
            Some(&Object::Reference(resources_ref))
        );
    }

    #[test]
    fn adjust_appearance_stream_no_resources_is_noop() {
        let mut pdf = open_minimal();
        let ap_ref = set_stream(&mut pdf, 4, &[], b"/F1 18 Tf");
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1 18 Tf");
        assert!(stream.dict.get("Resources").is_none());
    }

    #[test]
    fn adjust_appearance_stream_rewrites_content_and_privatizes_indirect_resources() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let resources_ref = set_dict(
            &mut pdf,
            3,
            &[(
                "Font",
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("F1", Object::Reference(font_ref));
                    d
                }),
            )],
        );
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Reference(resources_ref))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf");
        let new_resources_ref = stream
            .dict
            .get_ref("Resources")
            .expect("Resources should stay an indirect reference");
        assert_ne!(
            new_resources_ref, resources_ref,
            "must be a FRESH private object, not the original shared one"
        );

        // The private copy has ONLY the renamed key, pointing at the same
        // font object as before.
        let new_font = pdf
            .resolve(new_resources_ref)
            .unwrap()
            .into_dict()
            .unwrap()
            .get("Font")
            .and_then(Object::as_dict)
            .unwrap()
            .clone();
        assert_eq!(new_font.get("F1_1"), Some(&Object::Reference(font_ref)));
        assert!(new_font.get("F1").is_none());

        // The ORIGINAL shared /DR-copy object is untouched.
        let orig_font = pdf
            .resolve(resources_ref)
            .unwrap()
            .into_dict()
            .unwrap()
            .get("Font")
            .and_then(Object::as_dict)
            .unwrap()
            .clone();
        assert_eq!(orig_font.get("F1"), Some(&Object::Reference(font_ref)));
    }

    #[test]
    fn adjust_appearance_stream_direct_resources_stays_direct() {
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get("F1_1"), Some(&Object::Reference(font_ref)));
    }

    #[test]
    fn adjust_appearance_stream_ensures_and_drops_empty_category() {
        // dr_map has a rename recorded under /ExtGState, but this AP
        // stream's own /Resources never had an /ExtGState entry at all: it
        // must be force-inserted (to unshare/exist), found empty (nothing
        // to rename into it), and then dropped — never left behind as a
        // stray empty sub-dictionary in the output.
        let mut pdf = open_minimal();
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(Dictionary::new()));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"q Q",
        );
        let dr_map = dr_map_with(b"ExtGState", b"GS1", b"GS1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("expected direct Resources dict");
        assert!(
            resources.get("ExtGState").is_none(),
            "empty /ExtGState sub-dict must be dropped, not left behind"
        );
        // The unrelated, pre-existing (empty) /Font sub-dict is untouched —
        // it wasn't force-inserted by this call, but since it's ALSO
        // empty, `merge_resources_shallow`'s qpdf counterpart would still
        // drop it via the same "remove empty subdictionaries" step, so it
        // must be dropped here too, matching qpdf iterating every
        // subdictionary of the resulting /Resources, not just the ones
        // `dr_map` touched.
        assert!(resources.get("Font").is_none());
    }
}
