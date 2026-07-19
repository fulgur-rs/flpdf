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
/// A match requires only that `dr_map` records a rename for that name under
/// the operator's resource category (populated by `merge_resources_shallow`
/// when the source `/DR`'s entry collided with an existing destination
/// entry under the same name) — there is NO additional check that the name
/// is actually present in this stream's own (local) `/Resources`
/// sub-dictionary. An earlier version of this function added such a
/// presence guard as a defensive measure, mirroring the one
/// `crate::overlay_annotations::adjust_default_appearance` applies to `/DA`
/// strings; it was removed (roborev PR #490 iter-3 finding 4) after fetching
/// qpdf's actual `ResourceReplacer`/`ResourceFinder` source
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc`, `libqpdf/ResourceFinder.cc`)
/// and confirming qpdf's `ResourceReplacer::handleToken` rewrites purely
/// from its precomputed `to_replace` map (`dr_map` crossed with the
/// content-stream scan) with no check against the stream's local
/// `/Resources` at all. The guard was not a no-op: an appearance stream can
/// reference a name that resolves only through the INHERITED
/// `/AcroForm/DR` (no local `/Resources` entry for it at all), and qpdf
/// still rewrites that token — the guard was blocking exactly that case.
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
/// once the `ID` operator is seen, everything up to the terminating `EI` is
/// copied through byte-for-byte without being fed to the operand/operator
/// lexer, so binary image data that happens to contain a byte sequence
/// resembling a resource-name operand (e.g. `/F1 18 Tf`) is never mistaken
/// for one.
///
/// Locating that terminating `EI` is itself two-stage, matching qpdf's
/// `Tokenizer::findEI`: a delimiter-bounded `EI` is found first
/// ([`next_delimiter_bounded_ei`]), then [`ei_lookahead_passes`] reads up to
/// the next 10 tokens (or EOF) to check the candidate isn't itself binary
/// image data that happens to look like a delimiter-bounded `EI`. See
/// [`find_inline_image_ei`] for the full search/fallback order.
///
/// Returns `content.to_vec()` verbatim, without scanning, when `dr_map` is
/// empty (the common case: no placement recorded a rename on this dest
/// page).
pub(crate) fn resource_replacer(content: &[u8], dr_map: &DrMap) -> Vec<u8> {
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
            let ei_pos = find_inline_image_ei(content, data_start);
            out.extend_from_slice(&content[data_start..ei_pos]);
            pos = ei_pos;
            continue;
        }
        if let Some(rtype) = resource_type_for_operator(op) {
            if let Some((out_start, out_end, name)) = last_name.take() {
                let renamed = dr_map.category(rtype).and_then(|m| m.get(name.as_slice()));
                if let Some(new_name) = renamed {
                    let mut replacement = Vec::with_capacity(new_name.len() + 1);
                    replacement.push(b'/');
                    crate::object::write_name_escaped(&mut replacement, new_name);
                    out.splice(out_start..out_end, replacement);
                }
            } // cov:ignore: control-flow marker — llvm-cov instrumentation artifact
        }
    }
    out
}

/// Locate the `EI` that terminates inline image data starting at
/// `data_start`, matching qpdf's `Tokenizer::findEI`
/// (`libqpdf/QPDFTokenizer.cc`): a naive first-match search is wrong
/// because binary image data can itself contain a byte sequence that looks
/// exactly like a delimiter-bounded `EI`.
///
/// For every candidate found by [`next_delimiter_bounded_ei`],
/// [`ei_lookahead_passes`] reads up to the next 10 tokens (or EOF) and
/// rejects the candidate if any of them looks like it is still part of
/// image data rather than real content — qpdf's assumption is that at
/// least 10 tokens always separate one inline image's `EI` from the next
/// `BI`/`ID`, since `/W`, `/H`, `/BPC`, `/CS`, `BI`, and `ID` are all
/// required in between. The first candidate that passes wins. If every
/// candidate found is rejected, the LAST one found is used anyway — qpdf's
/// own fallback ("If we get to the end without finding one, return the
/// last EI we found"), which is why a rejected candidate can still end up
/// as the boundary and hand the bytes after it back to normal content
/// scanning. If no delimiter-bounded `EI` exists at all, everything to EOF
/// is treated as opaque data — a truncated/malformed stream, handled the
/// same tolerant way as every other recovery path in this scanner: never
/// panic or hang.
fn find_inline_image_ei(content: &[u8], data_start: usize) -> usize {
    let mut last_candidate: Option<usize> = None;
    let mut search_pos = data_start;
    while let Some(i) = next_delimiter_bounded_ei(content, data_start, search_pos) {
        last_candidate = Some(i);
        if ei_lookahead_passes(&content[i + 2..]) {
            return i;
        }
        search_pos = i + 2;
    }
    last_candidate.unwrap_or(content.len())
}

/// Find the next `EI` at or after `from` that is bounded by whitespace (or
/// the start of the inline image data, `data_start`) before it, and by
/// whitespace, a delimiter, or EOF after it. Returns the byte offset of the
/// `E`, or `None` if no such `EI` exists anywhere in the rest of `content`.
fn next_delimiter_bounded_ei(content: &[u8], data_start: usize, from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < content.len() {
        if content[i] == b'E'
            && content[i + 1] == b'I'
            && (i == data_start || is_ws(content[i - 1]))
            && (i + 2 >= content.len() || is_ws(content[i + 2]) || is_delimiter(content[i + 2]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// qpdf's secondary disambiguation heuristic for [`find_inline_image_ei`]:
/// read up to 10 tokens (or EOF) from `rest` — the bytes immediately after a
/// candidate `EI` — matching `Tokenizer::findEI`'s inner lookahead loop
/// (`libqpdf/QPDFTokenizer.cc`). Whitespace between tokens is free (does not
/// consume a slot); a `%` comment consumes one slot but is never itself
/// "bad" (matching qpdf's own tokenizer, which does not flag comments).
///
/// A candidate is rejected (this function returns `false`) as soon as any
/// lookahead token is:
/// - a malformed operand (an unterminated string, an invalid array/dict, …)
///   — the shared object lexer's `Err` case, or
/// - a stray delimiter that does not start a recognised operand (qpdf's
///   `tt_bad`), or
/// - a bare "word" (an operator-like keyword, or any other run of
///   non-whitespace, non-delimiter bytes) containing a byte outside the
///   printable 7-bit range (compared as a SIGNED byte, matching qpdf, so any
///   byte `>= 0x80` counts, not just ASCII control codes), or mixing an
///   alphabetic/`*` character with any other character — both patterns are
///   qpdf's signal that this "word" is more likely raw image bytes than a
///   real content-stream keyword.
///
/// Hitting EOF before 10 tokens, or getting through all 10 without
/// triggering either condition, means the candidate is accepted.
fn ei_lookahead_passes(rest: &[u8]) -> bool {
    let mut pos = 0usize;
    for _ in 0..10 {
        while pos < rest.len() && is_ws(rest[pos]) {
            pos += 1;
        }
        if pos >= rest.len() {
            return true;
        }
        let byte = rest[pos];
        if byte == b'%' {
            while pos < rest.len() && !matches!(rest[pos], b'\n' | b'\r') {
                pos += 1;
            }
            continue;
        }
        if byte == b'/'
            || byte == b'('
            || byte == b'<'
            || byte == b'['
            || matches!(byte, b'+' | b'-' | b'.' | b'0'..=b'9')
        {
            let mut parser = Parser::new_no_reference(&rest[pos..]);
            match parser.parse_one_object() {
                Ok(_) => {
                    pos += parser.position();
                    continue;
                }
                Err(_) => return false,
            }
        }
        if is_delimiter(byte) {
            return false;
        }
        let start = pos;
        while pos < rest.len() && !is_ws(rest[pos]) && !is_delimiter(rest[pos]) {
            pos += 1;
        }
        let mut found_alpha = false;
        let mut found_other = false;
        for &ch in &rest[start..pos] {
            if ch.is_ascii_alphabetic() || ch == b'*' {
                found_alpha = true;
            } else if (ch as i8) < 32 {
                return false;
            } else {
                found_other = true;
            }
        }
        if found_alpha && found_other {
            return false;
        }
    }
    true
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
///    rename for: resolve that category's sub-dictionary (an indirect
///    `/Font <ref>`-style sub-dictionary is resolved the same as a direct
///    one — an appearance stream's own `/Resources` can name either shape),
///    inserting an empty one if absent — mirrors merging in qpdf's
///    per-category empty `merge_with` dict, whose only effect at this stage
///    is to force the sub-dictionary to exist and be unshared.
/// 3. Rename every `old_key` present in that sub-dictionary to `dr_map`'s
///    `new_key`, keeping the same value, matching qpdf's rename loop
///    verbatim (`libqpdf/QPDFAcroFormDocumentHelper.cc:781-803`, fetched and
///    read line-by-line for this port — roborev PR #490 iter-3 finding 1).
///    This is a TWO-PHASE algorithm, not a single pass:
///    - **Phase 1** mutates the sub-dictionary key-by-key, IN PLACE, reading
///      `existing_new`/`old_val` from the LIVE, currently-mutating
///      sub-dictionary — exactly like qpdf. A `new_key` that already names a
///      value gets that value staged into a local `merge_with` dict (NOT
///      immediately renamed) before `old_key`'s value moves into `new_key`.
///      Because phase 1 reads live state, a LATER rename entry in the same
///      category CAN observe an EARLIER entry's freshly-written key — e.g.
///      `dr_map` recording both `F1->F1_1` and `F1_1->F1_1_1` for the same
///      category means the second entry's `old_key` lookup (`F1_1`) finds
///      the value phase 1 JUST moved there from `F1`, not the sub-dict's
///      true original `F1_1` occupant. This matches qpdf bug-for-bug: a
///      snapshot-based rewrite that avoided this "reprocessing" would
///      itself be the byte-identical divergence (see the
///      `adjust_appearance_stream_rename_chain_*` tests below, which encode
///      the resulting cross-wired names as the correct, qpdf-verified
///      output).
///    - **Phase 2** re-merges the staged `merge_with` dict back into the
///      (now phase-1-mutated) sub-dictionary, matching qpdf's
///      `resources.mergeResources(merge_with, &dr_map)` (`:807`, itself
///      `QPDFObjectHandle::mergeResources`'s generic dict-merge-with-
///      conflicts algorithm). A staged value whose slot phase 1 already
///      vacated re-lands under its own original name, no conflict. A
///      GENUINE new conflict (the slot is occupied by something else) is
///      resolved the same way `crate::overlay_annotations::merge_resources_shallow`
///      resolves the top-level `/DR` merge conflict: reuse a key that
///      already names the SAME object elsewhere in the sub-dictionary (by
///      [`ObjectRef`] identity — qpdf's `QPDFObjGen`-keyed `og_to_name`), or
///      else mint a fresh name via
///      [`crate::overlay_annotations::unique_dr_name`] and extend a
///      **per-call, cloned** copy of `dr_map` ([`DrMap::insert_rename`]) so
///      step 5's content rewrite also redirects any token that already said
///      the staged key. qpdf's `dr_map` parameter to this function is
///      itself passed **by value** (`:752`), which is exactly why growing a
///      local copy here cannot leak into another placement's shared
///      [`DrMap`].
/// 4. Drop any sub-dictionary left empty by step 3 (qpdf: "remove empty
///    subdictionaries").
/// 5. Rewrite the stream's decoded content via [`resource_replacer`], using
///    the per-call rename map extended by step 3. Steps 1–4 (the
///    `/Resources` dictionary rewrite) always run; step 5 is best-effort in
///    one direction only: if the content **cannot be decoded** at all (most
///    commonly an unsupported `/Filter`), the content bytes are left
///    exactly as read and only the dictionary-level rename from steps 1–4
///    applies. This was verified directly against qpdf's source
///    (`libqpdf/QPDFAcroFormDocumentHelper.cc`, fetched and read for
///    roborev PR #490 iter-3 finding 3): the `/Resources` rename (steps 1–4
///    of this function) runs unconditionally BEFORE qpdf's equivalent
///    tokenize step, which is wrapped in its OWN `try`/`catch`
///    (`:824-849`) that turns a content-parse failure into a warning
///    without rolling back the rename already made to `/Resources` — qpdf
///    genuinely leaves the stream in this state (renamed dict, stale
///    content) rather than reverting it, so flpdf matches rather than
///    "fixing" it. If the content **decodes** but the rewritten bytes
///    cannot be **re-encoded under the original `/Filter`** (e.g.
///    `/LZWDecode`, which flpdf can decode but not re-encode — see
///    `crate::filters::apply_single_filter_encode`, decision
///    flpdf-9hc.7.2), the rewritten content is instead re-encoded as
///    `FlateDecode` and `/Filter`/`/DecodeParms` are replaced accordingly, so
///    the dictionary rename and the content tokens never disagree about a
///    resource's name. qpdf never needs this fallback: it installs
///    `ResourceReplacer` as a token filter once the content tokenizes, and
///    its writer re-serializes under its own default output filter
///    (`FlateDecode`, since qpdf has no LZW encoder either) rather than
///    reproducing the original filter's bytes — the Flate fallback here
///    mirrors that write-time re-serialization.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`]. A content decode or
/// re-encode failure is not one of them (see step 5 above) — a decode
/// failure is swallowed and leaves the content unchanged, matching qpdf
/// (verified against source, not just observed behavior — see step 5); a
/// re-encode failure is swallowed and instead re-encodes the rewritten
/// content as `FlateDecode` so it stays consistent with the `/Resources`
/// rename.
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

    // Per-call, owned copy of `dr_map` — mirrors qpdf's `dr_map` parameter
    // to `AcroForm::adjustAppearanceStream` being passed BY VALUE
    // (`libqpdf/QPDFAcroFormDocumentHelper.cc:752`). Phase 2 below extends
    // this LOCAL copy only, via [`DrMap::insert_rename`], so an extra
    // rename discovered while privatizing one stream's `/Resources` never
    // leaks into another placement's shared `DrMap`.
    let mut local_dr_map = dr_map.clone();

    for category in dr_map.categories() {
        let Some(renames) = dr_map.category(category) else {
            continue; // cov:ignore: category() looked up from categories()'s own keys — never None
        };
        let existing = resources.get(category).cloned();
        let mut subdict = match existing {
            Some(Object::Dictionary(d)) => d,
            Some(Object::Reference(r)) => dest.resolve(r)?.into_dict().unwrap_or_default(),
            _ => Dictionary::new(),
        };

        // PHASE 1 (qpdf `libqpdf/QPDFAcroFormDocumentHelper.cc:781-803`):
        // mutate `subdict` key-by-key, IN PLACE — `existing_new`/`old_val`
        // are read from the LIVE, currently-mutating `subdict`, matching
        // qpdf's `subdict.getKey(new_key)` / `subdict.getKey(old_key)`
        // exactly (both consult `subdict` at the moment of THIS iteration,
        // not a pre-loop snapshot). Any value a rename would otherwise
        // silently clobber is staged into `merge_with` (qpdf's own local of
        // that name) rather than lost outright.
        let mut merge_with = Dictionary::new();
        for (old_key, new_key) in renames {
            if let Some(existing_new) = subdict.get(new_key.as_slice()).cloned() {
                merge_with.insert(new_key.clone(), existing_new);
            }
            if let Some(existing_old) = subdict.remove(old_key) {
                subdict.insert(new_key.clone(), existing_old);
            }
        }

        // PHASE 2 (qpdf `:805-807`'s `resources.mergeResources(merge_with,
        // &dr_map)`): re-merge every staged, displaced value back in. Not a
        // simple re-insert — `QPDFObjectHandle::mergeResources`'s
        // conflicts-map algorithm applies again here, so a slot phase 1
        // left vacant re-lands its staged value verbatim, while a slot
        // phase 1 left OCCUPIED by something else is a genuine new
        // conflict, resolved exactly like the top-level `/DR` merge
        // conflict in `merge_resources_shallow`: reuse a key that already
        // names the SAME object elsewhere in `subdict` (an `ObjectRef`
        // identity scan, qpdf's `og_to_name`), else mint a fresh name.
        if merge_with.iter().next().is_some() {
            let mut ref_to_key: std::collections::HashMap<ObjectRef, Vec<u8>> =
                std::collections::HashMap::new();
            for (k, v) in subdict.iter() {
                if let Some(r) = v.as_ref_id() {
                    ref_to_key.insert(r, k.to_vec());
                }
            }
            let staged: Vec<(Vec<u8>, Object)> = merge_with
                .iter()
                .map(|(k, v)| (k.to_vec(), v.clone()))
                .collect();
            for (key, rval) in staged {
                if subdict.get(key.as_slice()).is_none() {
                    // The slot this value was staged under is free again
                    // (phase 1 vacated it) — no conflict, reinstate verbatim.
                    subdict.insert(key, rval);
                    continue;
                }
                let reused = rval.as_ref_id().and_then(|r| ref_to_key.get(&r).cloned());
                if let Some(existing_key) = reused {
                    // `existing_key == key` (no rename needed — the
                    // displaced value already sits under its own staged
                    // name) matches qpdf's `if (new_key != key)` guard
                    // around its `conflicts[rtype][key] = new_key` write.
                    if existing_key != key {
                        local_dr_map.insert_rename(category, key, existing_key);
                    }
                } else {
                    let fresh_name = crate::overlay_annotations::unique_dr_name(&key, &subdict)?;
                    subdict.insert(fresh_name.clone(), rval);
                    local_dr_map.insert_rename(category, key, fresh_name);
                }
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

    // Best-effort content rewrite (step 5, see doc comment above): a decode
    // failure here — most commonly an unsupported `/Filter` chain (e.g. an
    // image codec on an AP stream that also has a `/Resources` collision) —
    // must NOT propagate. qpdf wraps its equivalent tokenize step in a
    // `try`/`catch` that turns exactly this failure into a warning rather
    // than aborting; propagating an `Err` here would fail the WHOLE overlay
    // call chain over one unrelated AP stream, which real qpdf does not do.
    // On decode failure, `stream.data` is simply left as read — only the
    // `/Resources` dictionary rename above still applies (matching qpdf,
    // which does not roll back the rename either).
    if let Ok(decoded) = crate::filters::decode_stream_data(&stream.dict, &stream.data) {
        let new_decoded = resource_replacer(&decoded, &local_dr_map);
        match crate::filters::encode_stream_data(&stream.dict, &new_decoded) {
            Ok(encoded) => {
                // Keep `/Length` consistent with the rewritten body — the
                // rename may shrink or grow the compressed payload, and a
                // stale dict `/Length` here would leave the stream
                // structurally inconsistent (symmetric with the FlateDecode
                // fallback below, which already updates it).
                stream.dict.insert(
                    "Length",
                    Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
                );
                stream.data = encoded;
            }
            Err(_) => {
                // Re-encoding under the ORIGINAL `/Filter` failed — decodable
                // but not re-encodable filters are exactly `/LZWDecode`
                // (`crate::filters::apply_single_filter_encode`, decision
                // flpdf-9hc.7.2: "flpdf writes stream compression as
                // FlateDecode only ... qpdf has no LZW encoder either").
                // Leaving `stream.data` untouched here (the pre-fix
                // behavior) would strand the content on the OLD resource
                // names while `/Resources` above already has the NEW ones —
                // an inconsistent stream. Re-encode the rewritten content as
                // `FlateDecode` instead, mirroring how qpdf's writer would
                // re-serialize this same token-filtered content under its
                // own default output filter rather than reproducing LZW.
                // In-memory FlateDecode of already-decoded bytes does not
                // fail in practice, so no further fallback is attempted.
                let mut flate_dict = Dictionary::new();
                flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                if let Ok(encoded) = crate::filters::encode_stream_data(&flate_dict, &new_decoded) {
                    stream.dict.remove("DecodeParms");
                    stream
                        .dict
                        .insert("Filter", Object::Name(b"FlateDecode".to_vec()));
                    stream.dict.insert(
                        "Length",
                        Object::Integer(i64::try_from(encoded.len()).unwrap_or(i64::MAX)),
                    );
                    stream.data = encoded;
                }
            }
        }
    }

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

    // ---- resource_replacer -------------------------------------------------

    #[test]
    fn resource_replacer_empty_dr_map_is_identity() {
        let dr_map = DrMap::new();
        let content = b"/F1 18 Tf (hi) Tj";
        assert_eq!(resource_replacer(content, &dr_map), content);
    }

    #[test]
    fn resource_replacer_rewrites_tf_font_name() {
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content = b"/F1 18 Tf";
        assert_eq!(resource_replacer(content, &dr_map), b"/F1_1 18 Tf");
    }

    #[test]
    fn resource_replacer_name_not_in_dr_map_is_verbatim() {
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content = b"/F2 18 Tf";
        assert_eq!(resource_replacer(content, &dr_map), content);
    }

    #[test]
    fn resource_replacer_rewrites_even_when_name_absent_from_local_resources() {
        // dr_map has a rename, but this stream's own (local) /Resources
        // never had the old name at all — e.g. the AP stream relies on the
        // name resolving through the INHERITED /AcroForm/DR rather than its
        // own /Resources. qpdf's `ResourceReplacer::handleToken` rewrites
        // purely from its precomputed `to_replace` map (verified against
        // `libqpdf/ResourceFinder.cc` source — roborev PR #490 iter-3
        // finding 4); it has NO check against the stream's local
        // /Resources, so this must still rewrite. An earlier version of
        // this function added exactly such a presence guard and wrongly
        // left this token untouched.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content = b"/F1 18 Tf";
        assert_eq!(resource_replacer(content, &dr_map), b"/F1_1 18 Tf".to_vec());
    }

    #[test]
    fn resource_replacer_name_inside_string_literal_is_verbatim() {
        // `(F1)` is a STRING, not a Name — `Tj` is not in the operator
        // table either, so nothing here can match regardless.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content = b"(F1) Tj";
        assert_eq!(resource_replacer(content, &dr_map), content);
    }

    #[test]
    fn resource_replacer_preserves_whitespace_and_comments() {
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"  % a comment\n /F1   18  Tf  ";
        assert_eq!(
            resource_replacer(content, &dr_map),
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
        let content: &[u8] = b"/F1 18 Tf ) /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
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
        let content: &[u8] = b"(bad /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"(bad /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_do_xobject() {
        let dr_map = dr_map_with(b"XObject", b"Fx1", b"Fx1_1");
        assert_eq!(
            resource_replacer(b"q /Fx1 Do Q", &dr_map),
            b"q /Fx1_1 Do Q".to_vec()
        );
    }

    #[test]
    fn resource_replacer_gs_extgstate() {
        let dr_map = dr_map_with(b"ExtGState", b"GS1", b"GS1_1");
        assert_eq!(
            resource_replacer(b"/GS1 gs", &dr_map),
            b"/GS1_1 gs".to_vec()
        );
    }

    #[test]
    fn resource_replacer_sh_shading() {
        let dr_map = dr_map_with(b"Shading", b"Sh1", b"Sh1_1");
        assert_eq!(
            resource_replacer(b"/Sh1 sh", &dr_map),
            b"/Sh1_1 sh".to_vec()
        );
    }

    #[test]
    fn resource_replacer_cs_and_lowercase_cs_colorspace() {
        let dr_map = dr_map_with(b"ColorSpace", b"CS1", b"CS1_1");
        assert_eq!(
            resource_replacer(b"/CS1 CS", &dr_map),
            b"/CS1_1 CS".to_vec()
        );
        assert_eq!(
            resource_replacer(b"/CS1 cs", &dr_map),
            b"/CS1_1 cs".to_vec()
        );
    }

    #[test]
    fn resource_replacer_scn_and_lowercase_scn_pattern() {
        let dr_map = dr_map_with(b"Pattern", b"P1", b"P1_1");
        assert_eq!(
            resource_replacer(b"1 0 0 /P1 SCN", &dr_map),
            b"1 0 0 /P1_1 SCN".to_vec()
        );
        assert_eq!(
            resource_replacer(b"1 0 0 /P1 scn", &dr_map),
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
        assert_eq!(
            resource_replacer(b"/Span /P1 BDC", &dr_map),
            b"/Span /P1_1 BDC".to_vec()
        );
        assert_eq!(
            resource_replacer(b"/Span /P1 DP", &dr_map),
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
        let content: &[u8] =
            b"/F1 18 Tf BI /W 1 /H 1 /BPC 8 /CS /G ID \x01/F1 18 Tf\x02 EI /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"/F1_1 18 Tf BI /W 1 /H 1 /BPC 8 /CS /G ID \x01/F1 18 Tf\x02 EI /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_ei_followed_by_delimiter_not_whitespace() {
        // `EI` immediately followed by a delimiter (`/`, no intervening
        // whitespace) must still be recognised as the terminator — the
        // "after" check accepts EITHER whitespace OR a delimiter, matching
        // qpdf's own delimiter-bounded `EI` search.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI /W 1 ID \x01/F1 18 Tf\x02 EI/F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"BI /W 1 ID \x01/F1 18 Tf\x02 EI/F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_without_ei_copies_to_eof() {
        // A truncated/malformed inline image with no delimiter-bounded `EI`
        // at all: the fallback must treat everything from `ID` to the end
        // of `content` as opaque data rather than losing bytes or hanging,
        // and must NOT rewrite the `/F1` pattern embedded in it.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI /W 1 ID \x01/F1 18 Tf\x02";
        assert_eq!(resource_replacer(content, &dr_map), content);
    }

    #[test]
    fn resource_replacer_inline_image_rejects_fake_ei_via_lookahead() {
        // A delimiter-bounded "EI" sits INSIDE the image data, immediately
        // followed by a byte (0xFF) that is not valid 7-bit content —
        // qpdf's lookahead heuristic must reject it as still being part of
        // the image, and keep scanning for a LATER, real `EI`. A naive
        // first-match search (the pre-fix behaviour) would stop the image
        // right at the fake `EI` and hand the rest — including the
        // `/F1 18 Tf` lookalike operand embedded in the image data — to the
        // normal token scanner, wrongly renaming it. With the lookahead,
        // the embedded `/F1 18 Tf` must stay untouched, and only the real
        // tokens before `BI` and after the genuine `EI` are renamed.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"/F1 18 Tf BI /W 1 /H 1 /BPC 8 /CS /G ID \x01\x02 EI \xFFzzzz /F1 18 Tf \x05\x06 EI /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"/F1_1 18 Tf BI /W 1 /H 1 /BPC 8 /CS /G ID \x01\x02 EI \xFFzzzz /F1 18 Tf \x05\x06 EI /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_rejects_fake_ei_mixed_alpha_and_other_word() {
        // The lookahead token right after the fake `EI` is `ab1`: it mixes
        // alphabetic characters (`a`, `b`) with a non-alphabetic one (`1`),
        // which qpdf's heuristic treats as suspicious (real operators are
        // pure alphabetic runs, e.g. `Tf`, `cm`, `Do`) and rejects.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI ID \x01 EI ab1 more /F1 18 Tf \x02 EI /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"BI ID \x01 EI ab1 more /F1 18 Tf \x02 EI /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_rejects_fake_ei_stray_delimiter() {
        // The lookahead token right after the fake `EI` is a lone `)` — a
        // delimiter that does not start any recognised operand — which is
        // qpdf's `tt_bad` case and rejects the candidate.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI ID \x01 EI ) /F1 18 Tf \x02 EI /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"BI ID \x01 EI ) /F1 18 Tf \x02 EI /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_rejects_fake_ei_malformed_operand() {
        // The lookahead token right after the fake `EI` starts an operand
        // (`(`) but never terminates it before EOF — the shared object
        // lexer's `Err` case, which also rejects the candidate.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI ID \x01 EI (unterminated /F1 18 Tf \x02 EI /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"BI ID \x01 EI (unterminated /F1 18 Tf \x02 EI /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_lookahead_treats_comment_as_one_token() {
        // A `%` comment right after a candidate `EI` is neither "bad" nor a
        // suspicious "word" — it consumes one of the 10 lookahead slots and
        // the candidate is still accepted once EOF follows.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI ID \x01 EI %trailing comment";
        assert_eq!(resource_replacer(content, &dr_map), content);
    }

    #[test]
    fn resource_replacer_inline_image_lookahead_ten_clean_tokens_accepts() {
        // Ten consecutive well-formed, non-EOF tokens after a candidate
        // `EI` (never hitting the early EOF success check) must still
        // accept the candidate once the fixed lookahead budget is
        // exhausted without ever finding a bad or suspicious token.
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI ID \x01 EI 1 2 3 4 5 6 7 8 9 10 /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"BI ID \x01 EI 1 2 3 4 5 6 7 8 9 10 /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_inline_image_falls_back_to_last_rejected_candidate() {
        // Exactly one delimiter-bounded `EI` exists in the image data, and
        // it is REJECTED by the lookahead (the very next byte, 0xFF, is
        // non-printable). No further "EI" occurs anywhere afterward, so the
        // outer search exhausts without ever finding a passing candidate —
        // qpdf's own fallback ("If we get to the end without finding one,
        // return the last EI we found") means this rejected candidate is
        // STILL used as the boundary, handing everything from it onward
        // back to normal content scanning. That is observable here: the
        // `/F1 18 Tf` sitting AFTER the rejected `EI` gets renamed, proving
        // the fallback is the rejected candidate's position and not a
        // blanket "copy everything to EOF".
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        let content: &[u8] = b"BI ID \x01 EI \xFFbad /F1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"BI ID \x01 EI \xFFbad /F1_1 18 Tf".to_vec()
        );
    }

    #[test]
    fn resource_replacer_rewrites_a_rename_chain_independently_of_dict_state() {
        // `resource_replacer` only ever rewrites content tokens FROM
        // `dr_map` — it has no dependency on how `adjust_appearance_stream`
        // resolved any `/Resources` dictionary collisions along the way
        // (roborev PR #490 iter-3 finding 1's dict-side "reprocessing" is a
        // property of `adjust_appearance_stream`'s rename loop, not of this
        // function). A chained dr_map (`F1->F1_1`, `F1_1->F1_1_1`) simply
        // rewrites every matching token independently, left to right.
        let mut dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        dr_map.insert_rename(b"Font", b"F1_1".to_vec(), b"F1_1_1".to_vec());
        let content: &[u8] = b"/F1 18 Tf /F1_1 18 Tf";
        assert_eq!(
            resource_replacer(content, &dr_map),
            b"/F1_1 18 Tf /F1_1_1 18 Tf".to_vec()
        );
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

    #[test]
    fn adjust_appearance_stream_rewrites_content_when_category_subdict_is_indirect() {
        // The AP stream's own `/Resources` is direct, but ITS `/Font` entry
        // is itself an indirect reference (`/Font 6 0 R`) — a shape PDF
        // permits and `merge_resources_shallow` already resolves on the
        // /DR-merge side. Before the fix, the pre-rename snapshot the
        // membership guard consults still held the un-resolved
        // `Object::Reference`, `Object::as_dict` cannot see through it, and
        // the guard silently blocked the rewrite even though `/F1` really
        // was present under that indirect sub-dict.
        let mut pdf = open_minimal();
        let font_ref = ObjectRef::new(5, 0);
        pdf.set_object(font_ref, Object::Dictionary(Dictionary::new()));
        let font_dict_ref = ObjectRef::new(6, 0);
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(font_ref));
        pdf.set_object(font_dict_ref, Object::Dictionary(font_dict));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Reference(font_dict_ref));
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
        assert_eq!(font.get_ref("F1_1"), Some(font_ref));
        assert!(font.get("F1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_double_conflict_mints_fresh_local_name() {
        // The AP stream's own /Resources/Font already has BOTH `F1` and
        // `F1_1` as two DIFFERENT font objects. Renaming `F1` -> `F1_1`
        // (per dr_map) would silently clobber the pre-existing, unrelated
        // `F1_1` font if the collision were not detected. qpdf handles this
        // by minting a fresh name for the displaced value
        // (`libqpdf/QPDFAcroFormDocumentHelper.cc:791-807`) and extending
        // its local `dr_map` so content that already said `/F1_1` follows
        // it there too. The fresh name is `getUniqueResourceName("F1_1_",
        // ...)`'s first free suffix, `F1_1_1` — NOT `F1_2`, since the
        // minted-name base is the RENAME TARGET (`F1_1`), not the original
        // source name (`F1`).
        let mut pdf = open_minimal();
        let f1_font_ref = ObjectRef::new(5, 0);
        pdf.set_object(f1_font_ref, Object::Dictionary(Dictionary::new()));
        let f1_1_font_ref = ObjectRef::new(6, 0);
        pdf.set_object(f1_1_font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(f1_font_ref));
        font_dict.insert("F1_1", Object::Reference(f1_1_font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf /F1_1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert!(font.get("F1").is_none());
        assert_eq!(
            font.get_ref("F1_1"),
            Some(f1_font_ref),
            "the renamed slot now holds F1's original value"
        );
        assert_eq!(
            font.get_ref("F1_1_1"),
            Some(f1_1_font_ref),
            "the displaced original F1_1 value moved to the freshly minted name"
        );
    }

    #[test]
    fn adjust_appearance_stream_double_conflict_same_object_is_noop() {
        // /Resources/Font already has BOTH `F1` and `F1_1` pointing at the
        // SAME underlying font object — qpdf's `QPDFObjGen` identity check
        // treats this as already-resolved (the renamed slot would hold the
        // exact same object either way) and mints no fresh name at all.
        let mut pdf = open_minimal();
        let shared_font_ref = ObjectRef::new(5, 0);
        pdf.set_object(shared_font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(shared_font_ref));
        font_dict.insert("F1_1", Object::Reference(shared_font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        // `/F1` is renamed to `/F1_1`; the second `/F1_1` token is untouched
        // (no local rename was ever recorded for it), so both tokens end up
        // saying `/F1_1`.
        assert_eq!(stream.data, b"/F1_1 18 Tf /F1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert!(font.get("F1").is_none());
        assert!(
            font.get("F1_1_1").is_none(),
            "no fresh name should be minted for a same-object collision"
        );
        assert_eq!(font.get_ref("F1_1"), Some(shared_font_ref));
    }

    #[test]
    fn adjust_appearance_stream_unsupported_filter_is_non_fatal_noop() {
        // The AP stream's own /Resources/Font has "F2", never "F1" — a rename
        // recorded under dr_map for F1->F1_1 could never have matched this
        // stream's content even if it decoded successfully. Its content uses
        // CCITTFaxDecode: a real ISO 32000 stream filter, but one flpdf
        // intentionally never decodes (crate::filters::passthrough_codec_label
        // — an image/binary passthrough codec, preserved verbatim). Real
        // qpdf's AcroForm::adjustAppearanceStream wraps the equivalent
        // content-parse step in a try/catch that turns exactly this kind of
        // failure into a warning, not a hard error, so it must not propagate
        // here either and kill the whole overlay call chain over one
        // unrelated AP stream. The /Resources dict rename step still runs
        // (matching qpdf, which renames before its own try/catch), but since
        // there was nothing to rename, it is a no-op; the content bytes must
        // be left byte-for-byte exactly as read since flpdf cannot decode
        // them at all.
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F2", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"CCITTFaxDecode".to_vec())),
            ],
            b"\x00\x01opaque-ccitt-bytes",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        let result = adjust_appearance_stream(&mut pdf, ap_ref, &dr_map);
        assert!(
            result.is_ok(),
            "an undecodable AP stream content must not fail the whole call"
        );

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"\x00\x01opaque-ccitt-bytes",
            "content bytes must be left exactly as read when they cannot be decoded"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(
            font.get("F2"),
            Some(&Object::Integer(1)),
            "unrelated existing key is untouched"
        );
        assert!(font.get("F1").is_none());
        assert!(font.get("F1_1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_undecodable_filter_keeps_resources_rename_but_leaves_content_stale()
    {
        // Unlike the CCITT test above, this stream's own /Resources/Font
        // DOES have "F1" — a REAL collision, so the /Resources rename
        // (steps 1-4) is not a no-op this time. The content still cannot
        // be decoded (same CCITTFaxDecode passthrough codec), so step 5's
        // content rewrite cannot run at all. This asserts flpdf's ACTUAL
        // (verified) qpdf-matching behavior: qpdf performs the /Resources
        // rename BEFORE its own try/catch'd tokenize step
        // (`libqpdf/QPDFAcroFormDocumentHelper.cc:791-807` runs before
        // `:824-849`), and does NOT roll the rename back when the
        // subsequent tokenize fails — confirmed by fetching qpdf's actual
        // source for roborev PR #490 iter-3 finding 3, which proposed a
        // rollback; a rollback would have been the qpdf DIVERGENCE, so it
        // was declined and this test instead documents the verified,
        // matching (if internally inconsistent-looking) result: the dict
        // says "F1_1", the content still says "F1" — exactly like qpdf.
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"CCITTFaxDecode".to_vec())),
            ],
            b"\x00\x01/F1 opaque-ccitt-bytes",
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        let result = adjust_appearance_stream(&mut pdf, ap_ref, &dr_map);
        assert!(
            result.is_ok(),
            "an undecodable AP stream content must not fail the whole call"
        );

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"\x00\x01/F1 opaque-ccitt-bytes",
            "content bytes must be left exactly as read — qpdf does not roll \
             back a rename it already applied to /Resources just because the \
             later tokenize step failed"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(
            font.get("F1_1"),
            Some(&Object::Integer(1)),
            "the /Resources dict rename (steps 1-4) still applies even though \
             the content (step 5) could not be rewritten"
        );
        assert!(font.get("F1").is_none());
    }

    #[test]
    fn adjust_appearance_stream_rename_chain_matches_qpdf_verified_result() {
        // dr_map records a CHAIN within one category: F1->F1_1 AND,
        // independently, F1_1->F1_1_1 (both entries genuinely present in
        // dr_map at once — plausible when the top-level /DR merge assigns
        // F1_1 to a renamed F1 while a DIFFERENT source object separately
        // collides under the destination's own pre-existing F1_1). This
        // stream's own /Resources/Font has both F1 and F1_1 locally.
        //
        // qpdf's rename loop (`libqpdf/QPDFAcroFormDocumentHelper.cc:781-803`,
        // fetched and read for roborev PR #490 iter-3 finding 1) mutates the
        // sub-dictionary IN PLACE: processing F1->F1_1 first (dr_map is
        // sorted, "F1" < "F1_1") moves F1's value into the F1_1 slot,
        // displacing the true original F1_1 value into a `merge_with`
        // side-map. Processing F1_1->F1_1_1 next then reads the ALREADY-
        // overwritten F1_1 slot (now holding F1's value, not the true
        // original) and moves THAT into F1_1_1. The re-merge step
        // (`:805-807`) then re-lands the side-mapped, true original F1_1
        // value back into ITS OWN name, F1_1 — which is free again, since
        // phase 1 vacated it. The two resources end up CROSS-WIRED between
        // the dict and content: this is qpdf's actual, verified output for
        // this input, not a bug flpdf introduced — a "cleaner" pre-snapshot
        // rewrite would be the byte-identical divergence here.
        let mut pdf = open_minimal();
        let f1_font_ref = ObjectRef::new(5, 0);
        pdf.set_object(f1_font_ref, Object::Dictionary(Dictionary::new()));
        let f1_1_font_ref = ObjectRef::new(6, 0);
        pdf.set_object(f1_1_font_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(f1_font_ref));
        font_dict.insert("F1_1", Object::Reference(f1_1_font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F1 18 Tf /F1_1 18 Tf",
        );
        let mut dr_map = dr_map_with(b"Font", b"F1", b"F1_1");
        dr_map.insert_rename(b"Font", b"F1_1".to_vec(), b"F1_1_1".to_vec());

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"/F1_1 18 Tf /F1_1_1 18 Tf");
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert!(font.get("F1").is_none());
        assert_eq!(
            font.get_ref("F1_1"),
            Some(f1_1_font_ref),
            "the true original F1_1 value re-lands under its OWN name via the re-merge"
        );
        assert_eq!(
            font.get_ref("F1_1_1"),
            Some(f1_font_ref),
            "F1's value ends up under F1_1_1, having been reprocessed by the second rename entry"
        );
    }

    #[test]
    fn adjust_appearance_stream_reuses_existing_key_for_same_object_on_new_conflict() {
        // Phase 2's conflict resolution (`libqpdf/QPDFAcroFormDocumentHelper.cc:805-807`'s
        // `resources.mergeResources(merge_with, &dr_map)`, `QPDFObjectHandle::mergeResources`'s
        // `og_to_name` reuse) is exercised here: /Resources/Font has F1 and
        // F2 BOTH pointing at the SAME object, plus F3 at a different
        // object. dr_map renames F3->F2. Phase 1 moves F3's value into F2
        // (displacing F2's original occupant — which is the SAME object as
        // F1 — into `merge_with`). Phase 2 then finds F2 occupied (by F3's
        // moved-in value) and, instead of minting a fresh name for the
        // displaced value, notices it already lives under F1 (by
        // `ObjectRef` identity) and records an EXTRA dr_map redirect
        // (F2->F1) instead — no fresh name minted at all.
        let mut pdf = open_minimal();
        let shared_ref = ObjectRef::new(5, 0);
        pdf.set_object(shared_ref, Object::Dictionary(Dictionary::new()));
        let other_ref = ObjectRef::new(6, 0);
        pdf.set_object(other_ref, Object::Dictionary(Dictionary::new()));
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Reference(shared_ref));
        font_dict.insert("F2", Object::Reference(shared_ref));
        font_dict.insert("F3", Object::Reference(other_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[("Resources", Object::Dictionary(resources))],
            b"/F3 18 Tf /F2 18 Tf",
        );
        let dr_map = dr_map_with(b"Font", b"F3", b"F2");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();
        assert_eq!(
            stream.data, b"/F2 18 Tf /F1 18 Tf",
            "F3 (now under F2) rewrites to /F2; the original /F2 token must \
             follow its displaced value to wherever it actually ended up (F1), \
             not stay pointing at F2 (now a different object) or lose the rename"
        );
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get_ref("F1"), Some(shared_ref));
        assert_eq!(font.get_ref("F2"), Some(other_ref));
        assert!(font.get("F3").is_none());
        assert!(
            font.get("F2_1").is_none(),
            "no fresh name should be minted — the displaced value is REUSED \
             under its existing F1 name, not aliased under a new one"
        );
    }

    /// Pack literal bytes as a minimal `/LZWDecode` stream: each byte is its
    /// own literal code (codes 0-255 are always literal single-byte table
    /// entries per PDF §7.4.4), followed by EOD (257). Every code stays 9
    /// bits wide because so few codes are emitted the table never reaches
    /// the first width-bump threshold (511 entries under the default
    /// EarlyChange). flpdf has no LZW encoder (decision flpdf-9hc.7.2), so a
    /// test needing LZW-encoded *input* must synthesize it directly —
    /// mirrors `filters::tests::pack_lzw_9bit`, which cannot be reused here
    /// since it is private to that module.
    fn pack_lzw_9bit_literal(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf: u32 = 0;
        let mut bits: u32 = 0;
        let mut codes: Vec<u16> = bytes.iter().map(|&b| u16::from(b)).collect();
        codes.push(257); // EOD
        for code in codes {
            buf = (buf << 9) | u32::from(code);
            bits += 9;
            while bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        if bits > 0 {
            out.push((buf << (8 - bits)) as u8);
        }
        out
    }

    #[test]
    fn adjust_appearance_stream_lzw_reencode_failure_falls_back_to_flate() {
        // `/LZWDecode` is the one filter flpdf can decode but not re-encode
        // (crate::filters::apply_single_filter_encode, decision
        // flpdf-9hc.7.2). Unlike the CCITT test above, this stream's own
        // /Resources/Font DOES have "F1", so dr_map's F1->F1_1 rename is a
        // REAL rename, not a no-op. Before this fix, the /Resources rename
        // still applied but the content-rewrite step silently discarded the
        // token-replaced bytes on re-encode failure, leaving the content on
        // the stale "/F1" name while /Resources only had "F1_1" — an
        // inconsistent stream. This asserts the two stay consistent.
        let mut pdf = open_minimal();
        let mut font_dict = Dictionary::new();
        font_dict.insert("F1", Object::Integer(1));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_dict));

        let lzw_bytes = pack_lzw_9bit_literal(b"/F1 18 Tf");
        let ap_ref = set_stream(
            &mut pdf,
            4,
            &[
                ("Resources", Object::Dictionary(resources)),
                ("Filter", Object::Name(b"LZWDecode".to_vec())),
            ],
            &lzw_bytes,
        );
        let dr_map = dr_map_with(b"Font", b"F1", b"F1_1");

        adjust_appearance_stream(&mut pdf, ap_ref, &dr_map).unwrap();

        let stream = pdf.resolve(ap_ref).unwrap().into_stream().unwrap();

        // Dict-level rename (steps 1-4) applied, as always.
        let resources = stream
            .dict
            .get("Resources")
            .and_then(Object::as_dict)
            .expect("Resources should stay a direct (embedded) dictionary");
        let font = resources.get("Font").and_then(Object::as_dict).unwrap();
        assert_eq!(font.get("F1_1"), Some(&Object::Integer(1)));
        assert!(font.get("F1").is_none());

        // The content must agree: re-encoded as FlateDecode (flpdf cannot
        // re-encode LZW), with the resource token renamed to match.
        assert_eq!(
            stream.dict.get("Filter"),
            Some(&Object::Name(b"FlateDecode".to_vec())),
            "un-re-encodable /LZWDecode must fall back to /FlateDecode"
        );
        assert!(
            stream.dict.get("DecodeParms").is_none(),
            "stale LZW /DecodeParms must not survive the filter swap"
        );
        let decoded_content =
            crate::filters::decode_stream_data(&stream.dict, &stream.data).unwrap();
        assert_eq!(
            decoded_content, b"/F1_1 18 Tf",
            "content must reference the RENAMED name, consistent with /Resources"
        );
        let expected_length = i64::try_from(stream.data.len()).unwrap();
        assert_eq!(
            stream.dict.get("Length"),
            Some(&Object::Integer(expected_length)),
            "/Length must match the newly re-encoded bytes"
        );
    }
}
