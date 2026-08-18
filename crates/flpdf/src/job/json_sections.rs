//! qpdf correspondence: `QPDFJob::doJSONPages`, `doJSONPageLabels`, `doJSONOutlines`, `doJSONAcroform`, `doJSONAttachments`, and `doJSONEncrypt` section builders.
//! qpdf 11.9.0 source responsibility: `QPDFJob.cc:1030-1330`.
//!
//! These builders live below the command-level `job` boundary because qpdf
//! constructs them from `QPDFJob::doJSON`, while the generic JSON value and
//! stream serializers remain in [`crate::json_inspect`].

use crate::json::Json;
use crate::json_inspect::{
    json_array, json_dictionary, pdf_dest_to_json, pdf_object_to_json, ConvertError,
};
use crate::object::{Object, ObjectRef};
use crate::object_handle::ObjectHandle;
use crate::pdf_string::decode_pdf_text_string;
use crate::Pdf;
use std::io::{Read, Seek};

// ── build_pages_section ───────────────────────────────────────────────────────

/// Flatten a `/Contents` entry into a list of indirect-reference strings.
/// Handles three forms:
/// - `Object::Reference(r)` → `["N M R"]`
/// - `Object::Array([Reference, ...])` → each element as `"N M R"` (direct
///   streams in the array are silently skipped — they carry no ref string)
/// - `Object::Null` or absent → `[]`
///
/// Direct inline Streams outside an array have no object number and are
/// therefore skipped (spec-compliant PDFs use indirect refs for /Contents).
/// Collect the page's `/Contents` references as `"N M R"` strings.
///
/// PDF allows `/Contents` in several shapes:
///
/// 1. A direct Stream (rare; no ref to emit, returns `[]`).
/// 2. A `Reference` to a Stream → one entry.
/// 3. A `Reference` to an Array (`/Contents 12 0 R` where `12 0 obj [4 0 R 5 0 R]`)
///    → resolve the indirect array and recurse over its elements.
/// 4. A direct `Array` of References → one entry per Reference element.
///
/// In every variant the function emits the *original* reference strings, not
/// the wrapper array's ref number — that matches qpdf's `contents` output.
pub(crate) fn collect_content_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    content_handle: &ObjectHandle,
) -> Result<Vec<String>, ConvertError> {
    fn ref_string(r: ObjectRef) -> String {
        format!("{} {} R", r.number, r.generation)
    }

    // Direct array elements never get a further resolve pass here — only the
    // top-level indirect case below unwraps one level of indirection, so a
    // reference nested deeper inside an already-direct array is emitted as
    // its own ref string rather than followed.
    fn refs_in_direct_array(elems: &[ObjectHandle]) -> Vec<String> {
        elems
            .iter()
            .filter_map(|e| e.object_ref().map(ref_string))
            .collect()
    }

    if let Some(r) = content_handle.object_ref() {
        // Resolve to see whether the indirect object is a Stream (in which
        // case this ref itself is the content) or an Array of Stream refs
        // (in which case its elements are the content).
        pdf.resolve_object_handle(content_handle)?;
        if content_handle.as_stream_dict().is_some() {
            return Ok(vec![ref_string(r)]);
        }
        return match content_handle.as_array() {
            Some(elems) => Ok(refs_in_direct_array(&elems)),
            // /Contents pointing at anything else (Null, missing) → empty.
            None => Ok(vec![]),
        };
    }

    match content_handle.as_array() {
        Some(elems) => Ok(refs_in_direct_array(&elems)),
        // Null, missing, or direct Stream — emit empty list.
        None => Ok(vec![]),
    }
}

/// Collect image XObject reference strings for a single page.
///
/// Walks the inherited `/Resources /XObject` dictionary and, for each entry
/// whose resolved value is a Stream with `/Subtype /Image`, appends
/// `"N M R"` (the *original* reference string) to the result. Entries that
/// are direct inline Streams (no ref number) are skipped. The output is
/// sorted by XObject name (alphabetical byte-lex order).
pub(crate) fn collect_image_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: crate::ObjectRef,
) -> Result<Vec<String>, ConvertError> {
    let resources = match crate::pages::resolve_inherited_resources(pdf, page_ref) {
        Ok(Some(d)) => d,
        Ok(None) => return Ok(vec![]),
        Err(e) => return Err(ConvertError::PdfError(e.to_string())),
    };
    // Bridge: `crate::pages` is not part of this migration slice and still
    // returns a legacy `Dictionary`; lift it once here so the rest of this
    // function can use the ObjectHandle idiom like the rest of json_inspect.
    let resources_dict = pdf
        .lift_object_to_handle(&Object::Dictionary(resources))?
        .as_dictionary()
        .unwrap_or_default();

    // Resolve the /XObject sub-dictionary (may itself be indirect).
    let Some(xobject_handle) = resources_dict.get(b"/XObject".as_slice()) else {
        return Ok(vec![]);
    };
    pdf.resolve_object_handle(xobject_handle)?;
    let Some(xobject_dict) = xobject_handle.as_dictionary() else {
        return Ok(vec![]);
    };

    // Iterate in name (key) order — BTreeMap gives byte-lex order automatically.
    let mut image_refs: Vec<String> = Vec::new();
    for value in xobject_dict.values() {
        // Each XObject entry should be an indirect Reference.
        let Some(xobj_ref) = value.object_ref() else {
            // Direct inline stream — no ref string available, skip.
            continue;
        };
        pdf.resolve_object_handle(value)?;
        let Some(stream_dict) = value.as_stream_dict().and_then(|d| d.as_dictionary()) else {
            continue;
        };
        let is_image = match stream_dict.get(b"/Subtype".as_slice()) {
            Some(subtype_handle) => {
                pdf.resolve_object_handle(subtype_handle)?;
                subtype_handle
                    .as_name()
                    .is_some_and(|subtype| subtype.as_slice() == b"Image")
            }
            None => false,
        };
        if is_image {
            image_refs.push(format!("{} {} R", xobj_ref.number, xobj_ref.generation));
        }
    }
    Ok(image_refs)
}

/// Build the qpdf JSON v2 `"pages"` section.
///
/// Returns a [`Json`] array where each element is a JSON object with
/// keys in alphabetical order:
/// `contents`, `images`, `label`, `object`, `outlines`, `pageposfrom1`.
///
/// - `label` is always `null` (placeholder; not yet populated).
/// - `outlines` is always `[]` (placeholder; not yet populated).
///
/// # Errors
///
/// Returns a [`ConvertError`] if the page tree cannot be traversed or any
/// object resolution fails.
pub fn build_pages_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Json, ConvertError> {
    let page_refs =
        crate::pages::page_refs(pdf).map_err(|e| ConvertError::PdfError(e.to_string()))?;

    let mut entries: Vec<Json> = Vec::with_capacity(page_refs.len());

    for (idx, page_ref) in page_refs.into_iter().enumerate() {
        let pageposfrom1 = (idx as i64) + 1;
        let object_str = format!("{} {} R", page_ref.number, page_ref.generation);

        // Resolve the page dict to extract /Contents.
        let page_handle = pdf.get_object_handle(page_ref);
        pdf.resolve_object_handle(&page_handle)?;
        let page_dict = page_handle.as_dictionary().unwrap_or_default();
        let contents_handle = page_dict.get(b"/Contents".as_slice());
        let contents: Vec<Json> = match contents_handle {
            Some(c) => collect_content_refs(pdf, c)?
                .into_iter()
                .map(Json::make_string)
                .collect(),
            None => vec![],
        };

        // Collect image XObject refs from (inherited) Resources.
        let images: Vec<Json> = collect_image_refs(pdf, page_ref)?
            .into_iter()
            .map(Json::make_string)
            .collect();

        // Build page entry with keys in strict alphabetical order:
        // contents < images < label < object < outlines < pageposfrom1
        let entry = json_dictionary([
            ("contents".to_string(), json_array(contents)?),
            ("images".to_string(), json_array(images)?),
            // placeholder until flpdf-9hc.11.5 (page labels)
            ("label".to_string(), Json::make_null()),
            ("object".to_string(), Json::make_string(object_str)),
            // placeholder until flpdf-9hc.11.6 (outline back-references)
            ("outlines".to_string(), Json::make_array()),
            ("pageposfrom1".to_string(), Json::make_int(pageposfrom1)),
        ]);
        entries.push(entry?);
    }

    json_array(entries)
}

// ── build_acroform_section ────────────────────────────────────────────────────

/// Build the qpdf JSON v2 `acroform` section.
///
/// qpdf emits one entry for every Widget annotation encountered in page order,
/// not one entry for every node reachable from `/AcroForm/Fields`. The field
/// and annotation values are projected through their corresponding helper
/// boundaries so merged fields, inherited values, and orphan Widgets retain
/// qpdf's live ObjectHandle identity.
///
/// Correspondence: `QPDFJob::doJSONAcroform`
/// (`libqpdf/QPDFJob.cc:1159-1203`),
/// `QPDFAcroFormDocumentHelper::getWidgetAnnotationsForPage` and
/// `getFieldForAnnotation` (`libqpdf/QPDFAcroFormDocumentHelper.cc:197-232`),
/// `QPDFFormFieldObjectHelper` (`libqpdf/QPDFFormFieldObjectHelper.cc:29-285`),
/// and `QPDFAnnotationObjectHelper` (`libqpdf/QPDFAnnotationObjectHelper.cc:13-47`).
pub fn build_acroform_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Json, ConvertError> {
    // qpdf's page helper repairs and snapshots the page list before the
    // AcroForm helper starts its cached annotation-to-field analysis. Rust's
    // mutable borrow rules require the same sequencing explicitly.
    let page_refs = {
        let mut page_document = crate::PageDocumentHelper::new(pdf);
        page_document.get_all_pages()?
    };

    // Keep the helper alive for the whole page/widget walk so every lookup
    // uses one qpdf-shaped analysis cache. The handles are collected before
    // the field and annotation accessors borrow the Pdf again below.
    let (has_acroform, need_appearances, widgets) = {
        let mut acroform = crate::AcroFormDocumentHelper::new(pdf);
        let has_acroform = acroform.has_acro_form()?;
        let need_appearances = acroform.get_need_appearances()?;
        let mut widgets = Vec::new();

        for (page_index, page_ref) in page_refs.into_iter().enumerate() {
            for annotation in acroform.get_widget_annotations_for_page(page_ref)? {
                let field = acroform.get_field_for_annotation_handle(annotation.clone())?;
                widgets.push((page_index as i64 + 1, field, annotation));
            }
        }

        (has_acroform, need_appearances, widgets)
    };

    let fields = Json::make_array();
    for (pageposfrom1, field, annotation) in widgets {
        let parent = pdf_object_to_json(&field.try_get_key(b"/Parent")?)?;

        let (
            fieldtype,
            fieldflags,
            fullname,
            partialname,
            alternativename,
            mappingname,
            value,
            defaultvalue,
            quadding,
            ischeckbox,
            isradiobutton,
            ischoice,
            istext,
            choices,
        ) = {
            let mut field_helper =
                crate::FormFieldObjectHelper::from_object_handle(field.clone(), pdf);
            let fieldtype = field_helper
                .field_type()?
                .map(|name| String::from_utf8_lossy(&name).into_owned())
                .unwrap_or_default();
            let fieldflags = field_helper.flags()?;
            let fullname = field_helper.fully_qualified_name()?;
            let partialname = field_helper.partial_name()?;
            let alternativename = field_helper.alternative_name()?;
            let mappingname = field_helper.mapping_name()?;
            let value = field_helper.value()?;
            let defaultvalue = field_helper.default_value()?;
            let quadding = field_helper.quadding()?;
            let ischeckbox = field_helper.is_checkbox()?;
            let isradiobutton = field_helper.is_radio_button()?;
            let ischoice = field_helper.is_choice()?;
            let istext = field_helper.is_text()?;
            let choices = field_helper.choices()?;

            (
                fieldtype,
                fieldflags,
                fullname,
                partialname,
                alternativename,
                mappingname,
                value,
                defaultvalue,
                quadding,
                ischeckbox,
                isradiobutton,
                ischoice,
                istext,
                choices,
            )
        };

        let value = value
            .as_ref()
            .map(pdf_object_to_json)
            .transpose()?
            .unwrap_or_else(Json::make_null);
        let defaultvalue = defaultvalue
            .as_ref()
            .map(pdf_object_to_json)
            .transpose()?
            .unwrap_or_else(Json::make_null);

        let (mut appearancestate, annotationflags) = {
            let mut annotation_helper =
                crate::AnnotationObjectHelper::from_object_handle(annotation.clone(), pdf);
            (
                annotation_helper.get_appearance_state()?,
                annotation_helper.get_flags()?,
            )
        };
        // qpdf's getName() includes the leading slash. The shared annotation
        // helper intentionally exposes raw name bytes without it, so restore
        // qpdf's JSON spelling at this serialization boundary.
        if !appearancestate.is_empty() {
            appearancestate.insert(0, b'/');
        }
        let annotation = json_dictionary([
            ("object", pdf_object_to_json(&annotation)?),
            (
                "appearancestate",
                Json::make_string(String::from_utf8_lossy(&appearancestate).into_owned()),
            ),
            ("annotationflags", Json::make_int(annotationflags)),
        ])?; // cov:ignore: fixed annotation schema keys cannot trigger JsonError

        let field = json_dictionary([
            ("object", pdf_object_to_json(&field)?),
            ("parent", parent),
            ("pageposfrom1", Json::make_int(pageposfrom1)),
            ("fieldtype", Json::make_string(fieldtype)),
            ("fieldflags", Json::make_int(fieldflags)),
            ("fullname", Json::make_string(fullname)),
            ("partialname", Json::make_string(partialname)),
            ("alternativename", Json::make_string(alternativename)),
            ("mappingname", Json::make_string(mappingname)),
            ("value", value),
            ("defaultvalue", defaultvalue),
            ("quadding", Json::make_int(quadding)),
            ("ischeckbox", Json::make_bool(ischeckbox)),
            ("isradiobutton", Json::make_bool(isradiobutton)),
            ("ischoice", Json::make_bool(ischoice)),
            ("istext", Json::make_bool(istext)),
            (
                "choices",
                json_array(choices.into_iter().map(Json::make_string))?,
            ),
            ("annotation", annotation),
        ])?; // cov:ignore: fixed field schema keys cannot trigger JsonError
        fields
            .add_array_element(field)
            .map_err(|error| ConvertError::JsonError(error.to_string()))?;
    }

    json_dictionary([
        ("hasacroform", Json::make_bool(has_acroform)),
        ("needappearances", Json::make_bool(need_appearances)),
        ("fields", fields),
    ])
}

// ── build_pagelabels_section ──────────────────────────────────────────────────

/// Build the qpdf JSON v2 `"pagelabels"` section.
///
/// Returns a [`Json`] array where each element is a JSON object with
/// keys in alphabetical order: `index`, `label`.
///
/// Returns an empty [`Json`] array when the document has no `/PageLabels` entry.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails during tree walk.
pub fn build_pagelabels_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Json, ConvertError> {
    // qpdf's doJSONPageLabels obtains the full page list before checking
    // whether /PageLabels exists. Preserve both that validation side effect
    // and the observable everCalledGetAllPages metadata state.
    let page_count = crate::pages::page_refs(pdf)?.len();
    let entries = {
        let mut helper = crate::page_label_document_helper::PageLabelDocumentHelper::new(pdf);
        // qpdf's doJSONPageLabels gates only on hasPageLabels(): for a
        // zero-page document that still has /PageLabels,
        // getLabelsForPageRange(0, -1, 0, ...) still unconditionally pushes
        // one reconstructed leading label before its per-page loop (which
        // then never executes for end_idx < start_idx) --
        // QPDFPageLabelDocumentHelper.cc:54-90. A page-count shortcut here
        // would diverge from that single-entry output.
        if !helper.has_page_labels()? {
            return Ok(Json::make_array());
        }

        // QPDFJob::doJSONPageLabels asks the page-label helper for the effective
        // entries covering every page, rather than serializing raw number-tree
        // nodes. Keep the handles live through this call so /St adjustments,
        // explicit empty /P values, and unknown /S names retain qpdf semantics.
        let page_count = i64::try_from(page_count)
            .map_err(|_| ConvertError::PdfError("page count exceeds i64".to_string()))?;
        let mut entries = Vec::new();
        helper.get_labels_for_page_range(0, page_count - 1, 0, &mut entries)?;
        entries
    };

    let result: Result<Vec<Json>, ConvertError> = entries
        .into_iter()
        .map(|(idx, label)| {
            let label_json = pdf_object_to_json(&label)?;
            json_dictionary([
                ("index".to_string(), Json::make_int(idx)),
                ("label".to_string(), label_json),
            ])
        })
        .collect();

    json_array(result?)
}

// ── build_outlines_section ────────────────────────────────────────────────────

/// Project one live outline item into qpdf's JSON v2 shape.
fn outline_item_to_json<R: Read + Seek>(
    tree: &crate::OutlineTree,
    id: crate::OutlineId,
    page_numbers: &std::collections::BTreeMap<crate::ObjectRef, i64>,
    helper: &mut crate::OutlineDocumentHelper<'_, R>,
) -> Result<Json, ConvertError> {
    let item = &tree[id];
    let dest = pdf_dest_to_json(&item.dest(helper)?)?;
    let destpageposfrom1 = item
        .dest_page(helper)?
        .object_ref()
        .and_then(|reference| page_numbers.get(&reference).copied())
        .map(Json::make_int)
        .unwrap_or_else(Json::make_null);
    let count = item.count(helper)?;
    let title = item.title(helper)?;
    let mut kids = Vec::with_capacity(item.kids.len());
    for kid in item.kids.iter().copied() {
        kids.push(outline_item_to_json(tree, kid, page_numbers, helper)?);
    }
    let object = match item.source_ref {
        Some(reference) => Json::make_string(reference.to_string()),
        None => pdf_object_to_json(&item.object)?,
    };

    json_dictionary([
        ("dest".to_string(), dest),
        ("destpageposfrom1".to_string(), destpageposfrom1),
        ("kids".to_string(), json_array(kids)?),
        ("object".to_string(), object),
        ("open".to_string(), Json::make_bool(count >= 0)),
        ("title".to_string(), Json::make_string(title)),
    ])
}

/// Build the qpdf JSON v2 `"outlines"` section.
///
/// Returns a [`Json`] array where each element is a JSON object
/// representing one root-level outline item (with `kids` recursively
/// expanded).  Returns an empty [`Json`] array when the document has no
/// `/Outlines` entry or the outline dictionary has no `/First` child.
///
/// Each entry has keys in alphabetical order: `dest`, `destpageposfrom1`,
/// `kids`, `object`, `open`, `title`.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails.
pub fn build_outlines_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Json, ConvertError> {
    let page_numbers = crate::pages::page_refs(pdf)?
        .into_iter()
        .enumerate()
        .map(|(index, reference)| (reference, index as i64 + 1))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut helper = pdf.outline();
    let tree = helper.get_tree()?;
    let mut entries = Vec::with_capacity(tree.roots().len());
    for id in tree.roots().to_vec() {
        entries.push(outline_item_to_json(&tree, id, &page_numbers, &mut helper)?);
    }
    json_array(entries)
}

/// Parse a PDF date string (ISO 32000-1 §7.9.4) into an ISO 8601 string.
///
/// The PDF date format is `D:YYYYMMDDhhmmss±HH'mm'` (or `Z` for UTC).
/// Returns `None` if the bytes cannot be parsed as a date.
///
/// The input is required to be pure ASCII: PDF dates are an ASCII-only
/// format, and treating them as such avoids panicking on multibyte byte
/// boundaries when caller code passes a stray non-ASCII string.
pub(crate) fn parse_pdf_date(bytes: &[u8]) -> Option<String> {
    // PDF dates are ASCII-only. Reject any non-ASCII byte up front so the
    // byte-index slicing below cannot land in the middle of a UTF-8
    // multibyte sequence.
    if !bytes.is_ascii() {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;
    let s = s.strip_prefix("D:").unwrap_or(s);

    // Must have at least 4 chars for the year
    if s.len() < 4 {
        return None;
    }

    // Validate each fixed-width component is all digits before slicing it.
    let is_digits = |slice: &str| !slice.is_empty() && slice.bytes().all(|b| b.is_ascii_digit());

    let year = &s[0..4];
    if !is_digits(year) {
        return None;
    }

    // The fixed-width date prefix must end at one of the valid component
    // boundaries: YYYY, YYYYMM, YYYYMMDD, YYYYMMDDhh, YYYYMMDDhhmm, or
    // YYYYMMDDhhmmss. A trailing partial component (e.g. an odd 5th char in
    // "D:20261") is malformed; we refuse it rather than discarding the
    // dangling digits.
    let prefix_len = s.len().min(14);
    if !matches!(prefix_len, 4 | 6 | 8 | 10 | 12 | 14) {
        return None;
    }

    let month_default = "01";
    let day_default = "01";
    let zero_default = "00";

    let take = |start: usize, end: usize, fallback: &'static str| -> Option<&str> {
        if s.len() >= end {
            let slice = &s[start..end];
            if is_digits(slice) {
                Some(slice)
            } else {
                None
            }
        } else {
            Some(fallback)
        }
    };

    let month = take(4, 6, month_default)?;
    let day = take(6, 8, day_default)?;
    let hour = take(8, 10, zero_default)?;
    let minute = take(10, 12, zero_default)?;
    let second = take(12, 14, zero_default)?;

    // Numeric range validation so we don't emit ISO 8601 strings that
    // downstream parsers will reject (e.g. month=13, hour=24). All fields
    // are guaranteed to be 2-digit ASCII at this point.
    let in_range = |s: &str, lo: u8, hi: u8| -> bool {
        s.parse::<u8>().map(|n| n >= lo && n <= hi).unwrap_or(false)
    };
    if !in_range(month, 1, 12)
        || !in_range(day, 1, 31)
        || !in_range(hour, 0, 23)
        || !in_range(minute, 0, 59)
        || !in_range(second, 0, 59)
    {
        return None;
    }

    // Parse timezone. Trailing garbage (anything not empty / Z / z / +... /
    // -...) must yield None rather than silently defaulting to "Z", to keep
    // the function's "unparseable input -> None" contract honest.
    let tz_str = if s.len() > 14 { &s[14..] } else { "" };
    let tz = if tz_str.is_empty() || tz_str == "Z" || tz_str == "z" {
        "Z".to_string()
    } else if let Some(rest) = tz_str.strip_prefix('+') {
        parse_tz_offset('+', rest)?
    } else {
        let rest = tz_str.strip_prefix('-')?;
        parse_tz_offset('-', rest)?
    };

    Some(format!("{year}-{month}-{day}T{hour}:{minute}:{second}{tz}"))
}

/// Parse a timezone offset in the form `HH'mm'` or `HH` and return a string
/// like `+HH:MM`. Returns `None` for malformed offsets so callers can
/// propagate the failure up.
fn parse_tz_offset(sign: char, rest: &str) -> Option<String> {
    // Accept exactly one of: `HH'mm'`, `HH'mm`, `HHmm`, or `HH`.
    // Strip the single optional closing apostrophe so the remaining shapes
    // collapse to four; anything else (multiple trailing apostrophes,
    // garbage suffix, partial component) is rejected.
    let rest = rest.strip_suffix('\'').unwrap_or(rest);
    let (hh, mm) = if rest.len() == 5 && rest.as_bytes().get(2) == Some(&b'\'') {
        (&rest[0..2], &rest[3..5])
    } else if rest.len() == 4 {
        (&rest[0..2], &rest[2..4])
    } else if rest.len() == 2 {
        (rest, "00")
    } else {
        return None;
    };
    // Validate digits and numeric ranges for the tz offset.
    if !hh.chars().all(|c| c.is_ascii_digit()) || !mm.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hh_n = hh.parse::<u8>().ok()?;
    let mm_n = mm.parse::<u8>().ok()?;
    if hh_n > 23 || mm_n > 59 {
        return None;
    }
    // If +00:00, emit Z
    if sign == '+' && hh_n == 0 && mm_n == 0 {
        Some("Z".to_string())
    } else {
        Some(format!("{sign}{hh}:{mm}"))
    }
}

/// Convert raw bytes to lowercase hex string.
pub(crate) fn checksum_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Source of a filespec value found in the EmbeddedFiles name tree.
///
/// PDF name tree leaf values can be either an indirect Reference (the common
/// case) or a direct Dictionary embedded inline. Both shapes must produce an
/// `attachments` entry.
enum FilespecSource {
    Indirect(crate::ObjectRef),
    Direct(std::collections::BTreeMap<Vec<u8>, ObjectHandle>),
}

/// Build a JSON entry for one filespec dictionary.
///
/// Returns an object with keys in alphabetical order:
/// `description`, `filespec`, `names`, `preferredcontents`, `preferredname`, `streams`.
fn filespec_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filespec_ref: crate::ObjectRef,
) -> Result<Json, ConvertError> {
    let filespec_str = format!("{} {} R", filespec_ref.number, filespec_ref.generation);

    let filespec_handle = pdf.get_object_handle(filespec_ref);
    pdf.resolve_object_handle(&filespec_handle)?;
    let Some(filespec_dict) = filespec_handle.as_dictionary() else {
        // Malformed filespec — return a minimal entry
        return json_dictionary([
            ("description".to_string(), Json::make_null()),
            ("filespec".to_string(), Json::make_string(filespec_str)),
            ("names".to_string(), Json::make_dictionary()),
            ("preferredcontents".to_string(), Json::make_null()),
            ("preferredname".to_string(), Json::make_null()),
            ("streams".to_string(), Json::make_dictionary()),
        ]);
    };

    filespec_dict_to_json(pdf, &filespec_dict, Some(filespec_str))
}

/// Same as [`filespec_to_json`] but takes the filespec dictionary directly,
/// for the case where the name tree leaf value is a direct dictionary rather
/// than an indirect reference. When `filespec_str` is `Some`, it is used for
/// the `filespec` key; when `None`, that key emits JSON null because
/// no reference number exists.
fn filespec_dict_to_json<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filespec_dict: &std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
    filespec_str: Option<String>,
) -> Result<Json, ConvertError> {
    let filespec_value = match filespec_str {
        Some(s) => Json::make_string(s),
        None => Json::make_null(),
    };

    // description: /Desc decoded as PDF text string, bare (no u:/b: prefix)
    let description = match filespec_dict.get(b"/Desc".as_slice()) {
        Some(handle) => {
            pdf.resolve_object_handle(handle)?;
            handle
                .as_string()
                .map(|bytes| {
                    let s = decode_pdf_text_string(&bytes)
                        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
                    Json::make_string(s)
                })
                .unwrap_or_else(Json::make_null)
        }
        None => Json::make_null(),
    };

    // names: collect /F, /UF, /DOS, /Mac, /Unix — each decoded as PDF text string
    // Keys are in alphabetical order (they already are in BTreeMap).
    let name_keys = ["DOS", "F", "Mac", "UF", "Unix"];
    let mut names_pairs: Vec<(String, Json)> = Vec::new();
    for key in &name_keys {
        let key = crate::object_handle::canonical_dictionary_key(key.as_bytes());
        let Some(handle) = filespec_dict.get(&key) else {
            continue;
        };
        pdf.resolve_object_handle(handle)?;
        if let Some(bytes) = handle.as_string() {
            let s = decode_pdf_text_string(&bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
            names_pairs.push((
                String::from_utf8_lossy(&key).into_owned(),
                Json::make_string(s),
            ));
        }
    }

    // preferredname: /UF > /F > /Unix > /DOS > /Mac — matches qpdf's
    // `QPDFFileSpecObjectHelper::name_keys` priority order
    // (QPDFFileSpecObjectHelper.cc), which is *not* alphabetical (/DOS comes
    // before /Mac).
    let preferred_name_key_order = ["UF", "F", "Unix", "DOS", "Mac"];
    let mut preferredname = Json::make_null();
    for key in &preferred_name_key_order {
        let key = crate::object_handle::canonical_dictionary_key(key.as_bytes());
        let Some(handle) = filespec_dict.get(&key) else {
            continue;
        };
        pdf.resolve_object_handle(handle)?;
        if let Some(bytes) = handle.as_string() {
            let s = decode_pdf_text_string(&bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
            preferredname = Json::make_string(s);
            break;
        }
    }

    // /EF dictionary: embedded file stream refs, keyed by /F /UF /DOS /Mac /Unix
    let ef_dict: Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> =
        match filespec_dict.get(b"/EF".as_slice()) {
            Some(handle) => {
                pdf.resolve_object_handle(handle)?;
                handle.as_dictionary()
            }
            None => None,
        };

    // preferredcontents: /EF/UF > /EF/F > /EF/Unix > /EF/DOS > /EF/Mac, same
    // priority order as preferredname, taking the first entry that resolves
    // to an actual stream (qpdf's `getEmbeddedFileStream()` checks
    // `isStream()`, not merely that the key is present).
    let preferred_ef_key_order = ["UF", "F", "Unix", "DOS", "Mac"];
    let mut preferredcontents = Json::make_null();
    if let Some(ref ef) = ef_dict {
        for key in &preferred_ef_key_order {
            let key = crate::object_handle::canonical_dictionary_key(key.as_bytes());
            let Some(handle) = ef.get(&key) else {
                continue;
            };
            let Some(r) = handle.object_ref() else {
                continue;
            };
            pdf.resolve_object_handle(handle)?;
            if handle.as_stream_dict().is_some() {
                preferredcontents = Json::make_string(format!("{} {} R", r.number, r.generation));
                break;
            }
        }
    }

    // streams: for each key in /EF, build a stream-info object
    // Keys alphabetical: /DOS, /F, /Mac, /UF, /Unix
    let ef_key_order = ["DOS", "F", "Mac", "UF", "Unix"];
    let mut streams_pairs: Vec<(String, Json)> = Vec::new();

    if let Some(ref ef) = ef_dict {
        for key in &ef_key_order {
            let key = crate::object_handle::canonical_dictionary_key(key.as_bytes());
            let Some(stream_ref) = ef.get(&key).and_then(ObjectHandle::object_ref) else {
                continue;
            };

            let stream_handle = pdf.get_object_handle(stream_ref);
            pdf.resolve_object_handle(&stream_handle)?;
            let Some(stream_dict) = stream_handle
                .as_stream_dict()
                .and_then(|d| d.as_dictionary())
            else {
                continue;
            };

            // mimetype: /Subtype name → bare string (no "/" prefix), or null
            let mimetype = match stream_dict.get(b"/Subtype".as_slice()) {
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle
                        .as_name()
                        .map(|bytes| {
                            Json::make_string(String::from_utf8_lossy(&bytes).into_owned())
                        })
                        .unwrap_or_else(Json::make_null)
                }
                None => Json::make_null(),
            };

            // /Params sub-dict
            let params_dict: Option<std::collections::BTreeMap<Vec<u8>, ObjectHandle>> =
                match stream_dict.get(b"/Params".as_slice()) {
                    Some(handle) => {
                        pdf.resolve_object_handle(handle)?;
                        handle.as_dictionary()
                    }
                    None => None,
                };

            // checksum: /Params /CheckSum bytes → lowercase hex, or null
            let checksum = match params_dict
                .as_ref()
                .and_then(|p| p.get(b"/CheckSum".as_slice()))
            {
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle
                        .as_string()
                        .map(|bytes| Json::make_string(checksum_to_hex(&bytes)))
                        .unwrap_or_else(Json::make_null)
                }
                None => Json::make_null(),
            };

            // creationdate: /Params /CreationDate → ISO 8601, or null
            let creationdate = match params_dict
                .as_ref()
                .and_then(|p| p.get(b"/CreationDate".as_slice()))
            {
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle
                        .as_string()
                        .and_then(|bytes| parse_pdf_date(&bytes))
                        .map(Json::make_string)
                        .unwrap_or_else(Json::make_null)
                }
                None => Json::make_null(),
            };

            // modificationdate: qpdf's `QPDFJob::doJSONAttachments` reads
            // `/Params /CreationDate` for *both* `creationdate` and
            // `modificationdate` — a copy-paste bug (QPDFJob.cc:1319-1322 in
            // qpdf 11.9.0, still present on qpdf `main`). Per the
            // byte-identical mandate qpdf's behavior is replicated even where
            // it is buggy, so `/ModDate` is deliberately never read here.
            let modificationdate = creationdate.clone();

            // Stream entry keys: checksum, creationdate, mimetype, modificationdate
            let stream_entry = json_dictionary([
                ("checksum".to_string(), checksum),
                ("creationdate".to_string(), creationdate),
                ("mimetype".to_string(), mimetype),
                ("modificationdate".to_string(), modificationdate),
            ])?;
            streams_pairs.push((String::from_utf8_lossy(&key).into_owned(), stream_entry));
        }
    }

    json_dictionary([
        ("description".to_string(), description),
        ("filespec".to_string(), filespec_value),
        ("names".to_string(), json_dictionary(names_pairs)?),
        ("preferredcontents".to_string(), preferredcontents),
        ("preferredname".to_string(), preferredname),
        ("streams".to_string(), json_dictionary(streams_pairs)?),
    ])
}

/// Build the qpdf JSON v2 `"attachments"` section.
///
/// Returns a [`Json`] dictionary where each key is an EmbeddedFiles name-tree
/// entry name (decoded PDF string, bare without prefix) and each value is a
/// filespec entry object.
///
/// Returns an empty [`Json`] dictionary when the document has no `/Names/EmbeddedFiles`.
///
/// # Errors
///
/// Returns a [`ConvertError`] if any indirect object resolution fails.
pub fn build_attachments_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Json, ConvertError> {
    use crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

    // Resolve the Catalog.
    let catalog_ref = match pdf.root_ref() {
        Some(r) => r,
        None => return Ok(Json::make_dictionary()),
    };
    let catalog = pdf
        .resolve_borrowed(catalog_ref)
        .map_err(ConvertError::from)?;
    let mut catalog = match catalog {
        Object::Dictionary(dictionary) => dictionary.clone(),
        _ => return Ok(Json::make_dictionary()),
    };
    let names_val = catalog.get("Names").cloned();

    enum NamesLocation {
        Direct,
        Indirect(crate::ObjectRef),
    }

    let (names_location, mut names_dict) = match names_val {
        Some(Object::Dictionary(dictionary)) => (NamesLocation::Direct, dictionary),
        Some(source @ Object::Reference(source_ref)) => {
            let (terminal, terminal_ref) =
                crate::ref_chain::resolve_ref_chain(pdf, &source).map_err(ConvertError::from)?;
            match terminal.into_dict() {
                Some(dictionary) => (
                    NamesLocation::Indirect(terminal_ref.unwrap_or(source_ref)),
                    dictionary,
                ),
                None => return Ok(Json::make_dictionary()),
            }
        }
        _ => return Ok(Json::make_dictionary()),
    };

    // /EmbeddedFiles name tree root: keep the original object shape so the
    // shared walker can resolve an indirect root itself and track its
    // ObjectRef in the visited set (cycle guard on a self-referential root).
    let ef_root = match names_dict.get("EmbeddedFiles").cloned() {
        Some(v) => v,
        None => return Ok(Json::make_dictionary()),
    };

    let original_ef_root = ef_root.clone();
    let mut tree = crate::NameTree::new(ef_root, true);
    tree.set_max_depth(DEFAULT_MAX_PAGE_TREE_DEPTH);
    let entries = tree.as_map(pdf).map_err(ConvertError::from)?;
    if tree.root() != &original_ef_root {
        names_dict.insert("EmbeddedFiles", tree.into_root());
        match names_location {
            NamesLocation::Direct => {
                catalog.insert("Names", Object::Dictionary(names_dict));
                pdf.set_object(catalog_ref, Object::Dictionary(catalog));
            }
            NamesLocation::Indirect(names_ref) => {
                pdf.set_object(names_ref, Object::Dictionary(names_dict));
            }
        }
    }

    let mut raw_entries: Vec<(String, FilespecSource)> = Vec::with_capacity(entries.len());
    for (key_bytes, value) in entries {
        let source = match value {
            Object::Reference(object_ref) => FilespecSource::Indirect(object_ref),
            Object::Dictionary(dictionary) => {
                // Bridge: the NameTree walker (out of this migration's
                // scope) still yields a legacy Object; lift a direct leaf
                // dictionary once so filespec_dict_to_json can use the
                // ObjectHandle idiom like the rest of json_inspect.
                let dict = pdf
                    .lift_object_to_handle(&Object::Dictionary(dictionary))?
                    .as_dictionary()
                    .unwrap_or_default();
                FilespecSource::Direct(dict)
            }
            _ => continue,
        };
        raw_entries.push((String::from_utf8_lossy(&key_bytes).into_owned(), source));
    }

    // Sort by name (alphabetical)
    raw_entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Build the output object. Both indirect (Reference) and direct
    // (inlined Dictionary) filespec values yield an attachments entry.
    let mut pairs: Vec<(String, Json)> = Vec::new();
    for (name, source) in raw_entries {
        let entry = match source {
            FilespecSource::Indirect(filespec_ref) => filespec_to_json(pdf, filespec_ref)?,
            FilespecSource::Direct(dict) => filespec_dict_to_json(pdf, &dict, None)?,
        };
        pairs.push((name, entry));
    }

    json_dictionary(pairs)
}

// ── build_encrypt_section ─────────────────────────────────────────────────────

/// Determine the qpdf method string ("none", "RC4", "AESv2", "AESv3") for a
/// crypt filter name looked up from the /CF dictionary of `encrypt`.
///
/// Returns `"none"` only for the explicit `Identity` selector or when there
/// is no `/Encrypt` revision to derive a default from. When the selector is
/// absent or the looked-up filter has no `/CFM`, the method falls back to
/// the revision-based default used by the reader and the qpdf handler:
///
/// - `/R >= 5` → `"AESv3"`
/// - `/R == 4` → `"AESv2"`
/// - everything else (legacy) → `"RC4"`
pub(crate) fn cf_method_string<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    encrypt: &std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
    selector: Option<&str>,
) -> Result<&'static str, ConvertError> {
    fn revision_default<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        encrypt: &std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
    ) -> Result<&'static str, ConvertError> {
        let r = match encrypt.get(b"/R".as_slice()) {
            Some(handle) => {
                pdf.resolve_object_handle(handle)?;
                handle.as_integer()
            }
            None => None,
        };
        Ok(match r {
            Some(r) if r >= 5 => "AESv3",
            Some(4) => "AESv2",
            _ => "RC4",
        })
    }

    let Some(selector) = selector else {
        return revision_default(pdf, encrypt);
    };
    if selector == "Identity" {
        return Ok("none");
    }
    // Look up the CFM entry inside /CF/<selector>
    let cf = match encrypt.get(b"/CF".as_slice()) {
        Some(handle) => {
            pdf.resolve_object_handle(handle)?;
            handle.as_dictionary()
        }
        None => None,
    };
    let Some(cf) = cf else {
        return revision_default(pdf, encrypt);
    };
    let selector = crate::object_handle::canonical_dictionary_key(selector.as_bytes());
    let filter = match cf.get(&selector) {
        Some(handle) => {
            pdf.resolve_object_handle(handle)?;
            handle.as_dictionary()
        }
        None => None,
    };
    let Some(filter) = filter else {
        return revision_default(pdf, encrypt);
    };
    let cfm = match filter.get(b"/CFM".as_slice()) {
        Some(handle) => {
            pdf.resolve_object_handle(handle)?;
            handle.as_name()
        }
        None => None,
    };
    Ok(match cfm {
        Some(cfm) => match cfm.as_slice() {
            b"AESV2" => "AESv2",
            b"AESV3" => "AESv3",
            b"V2" => "RC4",
            b"None" => "none",
            _ => revision_default(pdf, encrypt)?,
        },
        None => revision_default(pdf, encrypt)?,
    })
}

/// Read an optional name key from `dict` and return it decoded as UTF-8.
///
/// Returns `None` if the key is absent, the value is not a name, or the
/// name's bytes are not valid UTF-8 (matching the strict, non-lossy
/// decoding the original `Object`-based lookup used).
fn dict_name_str<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &std::collections::BTreeMap<Vec<u8>, ObjectHandle>,
    key: &str,
) -> Result<Option<String>, ConvertError> {
    let key = crate::object_handle::canonical_dictionary_key(key.as_bytes());
    let Some(handle) = dict.get(&key) else {
        return Ok(None);
    };
    pdf.resolve_object_handle(handle)?;
    Ok(handle
        .as_name()
        .and_then(|bytes| String::from_utf8(bytes).ok()))
}

/// Decode /P integer into per-capability booleans.
///
/// `p_raw` is the signed /P value. Per ISO 32000-1 §7.6.3.2 the bits are
/// tested after casting to u32 so that negative values (like -4) behave as
/// the expected all-bits-set value.
fn capabilities_from_p(p_raw: i32) -> Vec<(String, Json)> {
    let p = p_raw as u32;
    // All nine capabilities in alphabetical order (qpdf schema).
    let accessibility = (p & 0x0200) != 0;
    let extract = (p & 0x0010) != 0;
    let modify = (p & 0x0008) != 0;
    let modifyannotations = (p & 0x0020) != 0;
    let modifyassembly = (p & 0x0400) != 0;
    let modifyforms = (p & 0x0100) != 0;
    // modifyother mirrors modify (qpdf behaviour for standard handler)
    let modifyother = modify;
    let printhigh = (p & 0x0800) != 0;
    let printlow = (p & 0x0004) != 0;

    vec![
        ("accessibility".into(), Json::make_bool(accessibility)),
        ("extract".into(), Json::make_bool(extract)),
        ("modify".into(), Json::make_bool(modify)),
        (
            "modifyannotations".into(),
            Json::make_bool(modifyannotations),
        ),
        ("modifyassembly".into(), Json::make_bool(modifyassembly)),
        ("modifyforms".into(), Json::make_bool(modifyforms)),
        ("modifyother".into(), Json::make_bool(modifyother)),
        ("printhigh".into(), Json::make_bool(printhigh)),
        ("printlow".into(), Json::make_bool(printlow)),
    ]
}

/// All-true capabilities object used for plaintext (no /Encrypt) documents.
fn all_true_capabilities() -> Result<Json, ConvertError> {
    json_dictionary([
        ("accessibility", Json::make_bool(true)),
        ("extract", Json::make_bool(true)),
        ("modify", Json::make_bool(true)),
        ("modifyannotations", Json::make_bool(true)),
        ("modifyassembly", Json::make_bool(true)),
        ("modifyforms", Json::make_bool(true)),
        ("modifyother", Json::make_bool(true)),
        ("printhigh", Json::make_bool(true)),
        ("printlow", Json::make_bool(true)),
    ])
}

/// Build the `encrypt` section of the qpdf JSON v2 output.
///
/// Schema follows qpdf 11.x `--json --json-key=encrypt`:
/// - Plaintext / no `/Encrypt`: `encrypted: false`, all capabilities `true`,
///   all parameters 0 / "none".
/// - Encrypted: parameters from the `/Encrypt` dictionary; key is always
///   `null`; `recovereduserpassword` is always `null`.
///
/// The function reads the trailer's `/Encrypt` entry directly and does **not**
/// require any internal `EncryptionState` accessor, making it self-contained
/// inside `json_inspect`.
///
/// # Errors
///
/// Returns a [`ConvertError`] when the `/Encrypt` reference, or any of its
/// nested entries (`/V`, `/R`, `/P`, `/Length`, `/StmF`, `/StrF`, `/EFF`,
/// `/CF`, `/CFM`) that happen to be stored as indirect references, cannot be
/// resolved (i.e. an underlying I/O or parse error).
pub fn build_encrypt_section<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Json, ConvertError> {
    // Resolve /Encrypt via `trailer_key_handle`, not `trailer_handle`: the
    // latter lifts the *whole* trailer in one pass and degrades to a null
    // handle if any unrelated sibling entry's literal nesting exceeds the
    // inline-object bound, which would incorrectly report an otherwise-valid
    // /Encrypt entry as absent (plaintext). `trailer_key_handle` lifts only
    // this key's own value, so a deeply-nested sibling can't erase it.
    // `resolve_object_handle` is a no-op for a direct (inline) dictionary and
    // resolves an indirect reference in place, so a single call covers both
    // shapes; a present but non-dictionary value (any type, including an
    // unresolved reference) falls out of `as_dictionary()` as `None`,
    // matching the prior explicit `Object::Dictionary`/`Object::Reference`/
    // catch-all arms.
    let encrypt_handle = pdf.trailer_key_handle(b"Encrypt");
    pdf.resolve_object_handle(&encrypt_handle)?;
    let encrypt_dict = encrypt_handle.as_dictionary();

    let is_encrypted = pdf.is_encrypted();

    match encrypt_dict {
        None => {
            // Plaintext document: all defaults.
            let capabilities = all_true_capabilities()?;
            let parameters = json_dictionary([
                ("P", Json::make_int(0)),
                ("R", Json::make_int(0)),
                ("V", Json::make_int(0)),
                ("bits", Json::make_int(0)),
                ("filemethod", Json::make_string("none")),
                ("key", Json::make_null()),
                ("method", Json::make_string("none")),
                ("streammethod", Json::make_string("none")),
                ("stringmethod", Json::make_string("none")),
            ])?;
            json_dictionary([
                ("capabilities", capabilities),
                ("encrypted", Json::make_bool(false)),
                ("ownerpasswordmatched", Json::make_bool(false)),
                ("parameters", parameters),
                ("recovereduserpassword", Json::make_null()),
                ("userpasswordmatched", Json::make_bool(false)),
            ])
        }
        Some(ref enc) => {
            // Encrypted document: read V, R, P, /Length, CF methods. qpdf's
            // own encryption-parameter reads (`QPDFObjectHandle::isInteger`
            // et al.) transparently follow indirect references, so these
            // lookups resolve too rather than guarding against it.
            let v = match enc.get(b"/V".as_slice()) {
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle.as_integer().unwrap_or(0)
                }
                None => 0,
            };
            let r = match enc.get(b"/R".as_slice()) {
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle.as_integer().unwrap_or(0)
                }
                None => 0,
            };
            let p_raw = match enc.get(b"/P".as_slice()) {
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle.as_integer().map(|n| n as i32).unwrap_or(0)
                }
                None => 0,
            };
            let bits = match enc.get(b"/Length".as_slice()) {
                // Default key length when /Length is absent: 40 bits (V=1/2).
                None => 40,
                // Present: an integer value, or 0 for anything malformed.
                Some(handle) => {
                    pdf.resolve_object_handle(handle)?;
                    handle.as_integer().unwrap_or(0)
                }
            };

            // Determine method strings from /StmF, /StrF, /EFF selectors.
            let stmf = dict_name_str(pdf, enc, "StmF")?;
            let strf = dict_name_str(pdf, enc, "StrF")?;
            let eff = dict_name_str(pdf, enc, "EFF")?;

            let (streammethod, stringmethod, filemethod) = if v >= 4 {
                let sm = cf_method_string(pdf, enc, stmf.as_deref())?;
                let st = cf_method_string(pdf, enc, strf.as_deref())?;
                let fm = cf_method_string(pdf, enc, eff.as_deref().or(stmf.as_deref()))?;
                (sm, st, fm)
            } else if v == 1 || v == 2 {
                ("RC4", "RC4", "RC4")
            } else {
                ("none", "none", "none")
            };
            // top-level `method` mirrors streammethod (qpdf behaviour)
            let method = streammethod;

            let capabilities = json_dictionary(capabilities_from_p(p_raw))?;
            let parameters = json_dictionary([
                ("P", Json::make_int(p_raw as i64)),
                ("R", Json::make_int(r)),
                ("V", Json::make_int(v)),
                ("bits", Json::make_int(bits)),
                ("filemethod", Json::make_string(filemethod)),
                ("key", Json::make_null()),
                ("method", Json::make_string(method)),
                ("streammethod", Json::make_string(streammethod)),
                ("stringmethod", Json::make_string(stringmethod)),
            ])?;

            // ownerpasswordmatched / userpasswordmatched come from the
            // reader's authentication record so user-only-authenticated
            // documents do not falsely report owner=true.
            json_dictionary([
                ("capabilities", capabilities),
                ("encrypted", Json::make_bool(is_encrypted)),
                (
                    "ownerpasswordmatched",
                    Json::make_bool(pdf.owner_password_matched()),
                ),
                ("parameters", parameters),
                ("recovereduserpassword", Json::make_null()),
                (
                    "userpasswordmatched",
                    Json::make_bool(pdf.user_password_matched()),
                ),
            ])
        }
    }
}
