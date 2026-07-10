use flate2::write::ZlibEncoder;
use flate2::Compression;
use flpdf::{
    check_reader, filters, load_xref_and_trailer, parse_object, write_pdf, write_pdf_with_options,
    write_qdf, CompressStreams, Dictionary, Object, ObjectRef, ObjectStreamMode, Pdf, WriteOptions,
    XrefForm, XrefOffset,
};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::io::{BufReader, Cursor};
use std::process::Command;

#[test]
fn rewrites_minimal_pdf_to_valid_pdf() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

/// flpdf-9hc.31 regression: the plain (non-qdf) full-rewrite path must NOT
/// emit object 0 — the xref free-list head — as a body object. `object_refs()`
/// includes the free head `(0, 65535)`, but qpdf never writes it (or any
/// free/deleted entry) as a `0 65535 obj … endobj` body in any mode; it exists
/// only as an `f` row in the regenerated xref table. Leaking it shifts every
/// subsequent object offset and blocks bytes-identical output. The qdf path
/// already suppressed this (flpdf-9hc.6.10); this pins the plain path so the
/// guard can't silently regress to qdf-only.
#[test]
fn plain_full_rewrite_does_not_emit_object_0_as_body() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    // Plain full rewrite (no qdf).
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &opts).unwrap();

    // No body object for the free-list head, whether at the very start of the
    // file or after any preceding newline.
    assert!(
        !output.starts_with(b"0 65535 obj") && !contains_subslice(&output, b"\n0 65535 obj"),
        "plain full rewrite must not emit object 0 as a body object"
    );

    // The free-list head must still appear as an `f` row in the rebuilt xref
    // table — it is not dropped, just relocated out of the body.
    assert!(
        contains_subslice(&output, b"0000000000 65535 f "),
        "rebuilt xref table must still carry the free-list head row"
    );

    // And the result must remain a valid PDF.
    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

/// flpdf-9hc.31 regression (free/deleted parity): the plain full-rewrite path
/// must also drop a *non-zero* deleted object from the body, not just object 0.
/// `resolve()` returns `Object::Null` for a deleted ref, so without the guard
/// the plain path would emit `N 0 obj / null / endobj` for it — qpdf never does.
/// The deleted entry must survive only as an `f` row in the rebuilt xref table.
///
/// A *middle* object (3 of 4) is deleted so the rebuilt `/Size` still spans it
/// and it must appear as a free row — deleting the trailing object would just
/// shrink `/Size` past it and is a different (also-correct) outcome.
#[test]
fn plain_full_rewrite_does_not_emit_deleted_object_as_body() {
    // Source with four objects. The Catalog references both leaves (obj 3 and
    // obj 4) via /Aux keys so both are reachable from /Root and would normally
    // be emitted; deleting object 3 must therefore leave a genuine gap. (Under
    // the Catalog-first renumber an UNreferenced object is dropped like any
    // other unreachable object, so the deleted-vs-emitted distinction is only
    // observable when the object is reachable.)
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    let add = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };
    add(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Aux1 3 0 R /Aux2 4 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add(
        b"3 0 obj\n<< /Unreferenced 3 >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add(
        b"4 0 obj\n<< /Unreferenced 4 >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1,
        )
        .as_bytes(),
    );

    let deleted_ref = ObjectRef::new(3, 0);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    pdf.delete_object(deleted_ref);

    // Plain full rewrite (no qdf): the deleted object must not be re-emitted.
    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &opts).unwrap();

    // The deleted object's body must not be re-emitted. Objects are renumbered
    // Catalog-first, so identify the deleted object by its (unique) content
    // rather than its original number.
    assert!(
        !contains_subslice(&output, b"/Unreferenced 3"),
        "plain full rewrite must not emit the deleted object's body"
    );
    // The deleted object was reachable from /Root, so it received a new number
    // during the renumber walk but was skipped at emission. Its slot therefore
    // survives as a free `f` row, leaving a single non-zero gap in the rebuilt
    // table (object 4's body is still emitted under its new number).
    let latest_entries = parse_last_xref_entries(&output);
    let free_nonzero: Vec<u32> = latest_entries
        .iter()
        .filter(|(num, kind)| **num != 0 && **kind == b'f')
        .map(|(num, _)| *num)
        .collect();
    assert_eq!(
        free_nonzero.len(),
        1,
        "exactly one non-zero free xref slot must mark the deleted object \
         (entries: {latest_entries:?})"
    );
    // The surviving reachable object (obj 4's `/Unreferenced 4`) must still be
    // present, proving only the deleted object was dropped.
    assert!(
        contains_subslice(&output, b"/Unreferenced 4"),
        "the non-deleted reachable leaf must still be emitted"
    );

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

/// Returns true when `haystack` contains `needle` as a contiguous subslice.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Resolve the reference the Catalog stores under `/Metadata`.
///
/// Full-rewrite output is renumbered Catalog-first (flpdf-9hc.32), so a stream
/// referenced as the Catalog's `/Metadata` has an unstable object number;
/// navigate to it by reference rather than hardcoding a number.
fn metadata_stream_ref<R: std::io::Read + std::io::Seek>(pdf: &mut Pdf<R>) -> ObjectRef {
    let root = pdf.root_ref().expect("output must have a /Root");
    match pdf.resolve(root).expect("resolve /Root") {
        Object::Dictionary(d) => match d.get("Metadata") {
            Some(Object::Reference(r)) => *r,
            other => panic!("Catalog /Metadata must be a reference, got {other:?}"),
        },
        other => panic!("/Root must be a dictionary, got {other:?}"),
    }
}

/// flpdf-9hc.13.2: default (no-flag) /ID strategy, full-rewrite path.
///
/// First save of a source with no /ID emits a fresh two-element random /ID;
/// re-saving the result preserves element 1 (permanent identifier) verbatim
/// while element 2 (changing identifier) rotates — and the overall /ID differs
/// on every save (matching qpdf's default observable behaviour, ISO 32000-1
/// §14.4).
#[test]
fn default_id_random_first_save_then_preserves_element1_on_resave() {
    fn id_pair(trailer: &Dictionary) -> (Vec<u8>, Vec<u8>) {
        match trailer.get("ID") {
            Some(Object::Array(v)) if v.len() == 2 => {
                let s = |o: &Object| match o {
                    Object::String(b) => b.clone(),
                    other => panic!("expected /ID string, got {other:?}"),
                };
                (s(&v[0]), s(&v[1]))
            }
            other => panic!("expected 2-element /ID array, got {other:?}"),
        }
    }

    // Minimal source PDF with NO /ID in the trailer.
    let mut src = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();
    offsets.push(src.len());
    src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(src.len());
    src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let startxref = src.len();
    src.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    src.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offsets {
        src.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    src.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n").as_bytes(),
    );

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;

    // First save.
    let mut pdf1 = Pdf::open(Cursor::new(src.clone())).unwrap();
    let mut out1 = Vec::new();
    write_pdf_with_options(&mut pdf1, &mut out1, &opts).unwrap();
    let t1 = load_xref_and_trailer(&mut Cursor::new(&out1))
        .unwrap()
        .trailer;
    let (a1, b1) = id_pair(&t1);

    assert_eq!(a1.len(), 16, "first-save /ID[0] must be 16 bytes");
    assert_eq!(b1.len(), 16, "first-save /ID[1] must be 16 bytes");
    let pi: [u8; 16] = [
        0x31, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93, 0x23, 0x84, 0x62, 0x64, 0x33, 0x83, 0x27,
        0x95,
    ];
    assert_ne!(a1.as_slice(), &pi[..], "/ID[0] must not be the π constant");
    assert_ne!(b1.as_slice(), &pi[..], "/ID[1] must not be the π constant");
    assert!(a1.iter().any(|&c| c != 0), "/ID[0] must not be all-zero");
    assert!(b1.iter().any(|&c| c != 0), "/ID[1] must not be all-zero");

    // Re-save the first output.
    let mut pdf2 = Pdf::open(Cursor::new(out1.clone())).unwrap();
    let mut out2 = Vec::new();
    write_pdf_with_options(&mut pdf2, &mut out2, &opts).unwrap();
    let t2 = load_xref_and_trailer(&mut Cursor::new(&out2))
        .unwrap()
        .trailer;
    let (a2, b2) = id_pair(&t2);

    assert_eq!(
        a2, a1,
        "element 1 (permanent id) must be preserved on re-save"
    );
    assert_ne!(b2, b1, "element 2 (changing id) must rotate on re-save");
    assert_ne!((a1, b1), (a2, b2), "/ID must vary between saves");
}

/// flpdf-9hc.13.2 regression (coverage gap surfaced by roborev during the
/// stacked merge of #183): the **xref-stream** full-rewrite output branch
/// (`writer.rs` xref_dict path) must honour the same default-/ID contract as
/// the classic-xref branch — first save fresh random /ID, re-save preserves
/// element 1 and rotates element 2.  The pre-existing re-save test only
/// exercised the classic-xref path; this pins the xref-stream path so the
/// "permanent identifier preserved on re-save" guarantee can't silently
/// regress for xref-stream output (the path roborev flagged).
#[test]
fn default_id_random_xref_stream_full_rewrite_resave_preserves_element1() {
    fn id_pair(trailer: &Dictionary) -> (Vec<u8>, Vec<u8>) {
        match trailer.get("ID") {
            Some(Object::Array(v)) if v.len() == 2 => {
                let s = |o: &Object| match o {
                    Object::String(b) => b.clone(),
                    other => panic!("expected /ID string, got {other:?}"),
                };
                (s(&v[0]), s(&v[1]))
            }
            other => panic!("expected 2-element /ID array, got {other:?}"),
        }
    }

    // Source uses xref-stream form, so full_rewrite takes the xref_dict
    // (xref-stream output) branch — the exact path under review.
    let source = build_minimal_pdf_with_xref_stream();
    {
        let mut r = Cursor::new(&source);
        assert_eq!(
            load_xref_and_trailer(&mut r).unwrap().last_xref_form,
            XrefForm::Stream,
            "fixture must use xref stream form"
        );
    }

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true; // compress_streams defaults to Yes ⇒ xref-stream output

    // First save: fresh random two-element /ID.
    let mut pdf1 = Pdf::open(Cursor::new(source)).unwrap();
    let mut out1 = Vec::new();
    write_pdf_with_options(&mut pdf1, &mut out1, &opts).unwrap();
    let t1 = load_xref_and_trailer(&mut Cursor::new(&out1)).unwrap();
    assert_eq!(
        t1.last_xref_form,
        XrefForm::Stream,
        "first-save output must itself be xref-stream form (re-save also hits the xref_dict path)"
    );
    let (a1, b1) = id_pair(&t1.trailer);
    assert_eq!(a1.len(), 16, "first-save /ID[0] must be 16 bytes");
    assert_eq!(b1.len(), 16, "first-save /ID[1] must be 16 bytes");

    // Re-save the xref-stream output.
    let mut pdf2 = Pdf::open(Cursor::new(out1.clone())).unwrap();
    let mut out2 = Vec::new();
    write_pdf_with_options(&mut pdf2, &mut out2, &opts).unwrap();
    let t2 = load_xref_and_trailer(&mut Cursor::new(&out2)).unwrap();
    assert_eq!(
        t2.last_xref_form,
        XrefForm::Stream,
        "re-save output must also be xref-stream form, else the re-save no longer exercises the xref_dict path this test pins"
    );
    let (a2, b2) = id_pair(&t2.trailer);

    assert_eq!(
        a2, a1,
        "xref-stream path: element 1 (permanent id) must be preserved on re-save"
    );
    assert_ne!(
        b2, b1,
        "xref-stream path: element 2 (changing id) must rotate on re-save"
    );
}

/// flpdf-9hc.13.2: default /ID strategy on the **incremental** write path
/// (`write_pdf`, the most common entry point).  Same contract as the
/// full-rewrite variant: first save emits a fresh random /ID, re-save
/// preserves element 1 and rotates element 2.
#[test]
fn default_id_random_on_incremental_path_first_save_and_resave() {
    fn id_pair(trailer: &Dictionary) -> (Vec<u8>, Vec<u8>) {
        match trailer.get("ID") {
            Some(Object::Array(v)) if v.len() == 2 => {
                let s = |o: &Object| match o {
                    Object::String(b) => b.clone(),
                    other => panic!("expected /ID string, got {other:?}"),
                };
                (s(&v[0]), s(&v[1]))
            }
            other => panic!("expected 2-element /ID array, got {other:?}"),
        }
    }

    // Minimal source PDF with NO /ID in the trailer.
    let mut src = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();
    offsets.push(src.len());
    src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(src.len());
    src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let startxref = src.len();
    src.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    src.extend_from_slice(b"0000000000 65535 f \n");
    for o in &offsets {
        src.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    src.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n").as_bytes(),
    );

    // First save via the default incremental path (no options).
    let mut pdf1 = Pdf::open(Cursor::new(src.clone())).unwrap();
    let mut out1 = Vec::new();
    write_pdf(&mut pdf1, &mut out1).unwrap();
    let t1 = load_xref_and_trailer(&mut Cursor::new(&out1))
        .unwrap()
        .trailer;
    let (a1, b1) = id_pair(&t1);
    assert_eq!(a1.len(), 16);
    assert_eq!(b1.len(), 16);
    assert!(a1.iter().any(|&c| c != 0), "/ID[0] must not be all-zero");
    assert!(b1.iter().any(|&c| c != 0), "/ID[1] must not be all-zero");

    // Re-save the first output: element 1 preserved, element 2 rotated.
    let mut pdf2 = Pdf::open(Cursor::new(out1.clone())).unwrap();
    let mut out2 = Vec::new();
    write_pdf(&mut pdf2, &mut out2).unwrap();
    let t2 = load_xref_and_trailer(&mut Cursor::new(&out2))
        .unwrap()
        .trailer;
    let (a2, b2) = id_pair(&t2);
    assert_eq!(
        a2, a1,
        "element 1 (permanent id) must be preserved on incremental re-save"
    );
    assert_ne!(
        b2, b1,
        "element 2 (changing id) must rotate on incremental re-save"
    );
}

#[test]
fn write_pdf_preserves_source_bytes() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Marker (SENTINEL-UNTOUCHED) >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    assert!(output.len() > source.len());
    assert_eq!(&output[..source.len()], &source[..]);
    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains(&format!("/Prev {}", startxref)));
}

#[test]
fn write_pdf_twice_builds_valid_prev_chain() {
    let source_bytes = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let source_startxref = parse_startxref(&source_bytes);
    let mut generations = Vec::new();

    let mut source = source_bytes;

    for _ in 0..3 {
        let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
        let mut output = Vec::new();
        write_pdf(&mut pdf, &mut output).unwrap();

        let report = check_reader(Cursor::new(&output)).unwrap();
        assert!(
            report.valid,
            "generation {} diagnostics: {:?}",
            generations.len(),
            report.diagnostics.entries()
        );

        generations.push(output.clone());

        source = output.clone();
    }

    let mut prior_size = None;
    let mut prior_startxref = source_startxref;
    let mut prior_generations = None;

    for (generation, bytes) in generations.iter().enumerate() {
        let current_startxref = parse_startxref(bytes);
        let parsed = Pdf::open(Cursor::new(bytes)).unwrap();
        let current_generations = parse_last_xref_generations(bytes);
        let trailer = parsed.trailer();

        let prev = trailer.get("Prev");
        let Some(Object::Integer(previous_xref)) = prev else {
            panic!("generation {generation} is missing trailer /Prev")
        };

        assert_eq!(
            *previous_xref as u64, prior_startxref,
            "generation {generation} /Prev mismatch"
        );

        let size = trailer
            .get("Size")
            .and_then(as_integer)
            .expect("generation trailer missing integer /Size");
        if let Some(previous_size) = prior_size {
            assert!(
                size >= previous_size,
                "generation {generation} reduced /Size from {previous_size} to {size}",
            );
        }

        prior_size = Some(size);
        prior_startxref = current_startxref;

        if let Some(previous_generations) = prior_generations.as_ref() {
            for (object_number, previous_generation) in previous_generations {
                if let Some(current_generation) = current_generations.get(object_number) {
                    assert!(
                        current_generation >= previous_generation,
                        "generation {object_number} decreased from {previous_generation} to {current_generation} in generation {generation}",
                    );
                }
            }
        }

        prior_generations = Some(current_generations);
    }
}

#[test]
fn write_pdf_rewriting_chain_is_self_consistent_on_open() {
    let source = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let mut snapshots = Vec::new();
    let mut current = source;

    for _ in 0..2 {
        let mut pdf = Pdf::open(Cursor::new(current.clone())).unwrap();
        let mut output = Vec::new();
        write_pdf(&mut pdf, &mut output).unwrap();
        snapshots.push(output.clone());
        current = output;
    }

    let mut chain = Vec::<u64>::new();

    for generation in (1..snapshots.len()).rev() {
        let current = &snapshots[generation];
        let previous = &snapshots[generation - 1];

        let pdf = Pdf::open(Cursor::new(current)).unwrap();
        let prev = pdf
            .trailer()
            .get("Prev")
            .and_then(as_integer)
            .expect("trailer missing /Prev") as u64;

        chain.push(prev);
        assert_eq!(prev, parse_startxref(previous));

        let report = check_reader(Cursor::new(current)).unwrap();
        assert!(
            report.valid,
            "rewritten generation {generation} diagnostics: {:?}",
            report.diagnostics.entries()
        );
    }

    assert!(
        !chain.is_empty(),
        "expected /Prev values while validating rewritten chain",
    );
}

#[test]
fn write_pdf_emits_only_touched_objects() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let object = pdf.resolve(ObjectRef::new(1, 0)).unwrap();
    pdf.set_object(ObjectRef::new(1, 0), object);

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    assert!(output.len() > source.len());
    assert_eq!(&output[..source.len()], &source[..]);
    assert_eq!(count_substrings(&output, b"1 0 obj"), 2);
    assert_eq!(count_substrings(&output, b"2 0 obj"), 1);
}

#[test]
fn write_pdf_deletes_object_with_free_incremental_xref_entry() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Deleted false >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let deleted_ref = ObjectRef::new(3, 0);
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    pdf.delete_object(deleted_ref);

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let latest_entries = parse_last_xref_entries(&output);
    assert_eq!(latest_entries.get(&3), Some(&b'f'));
    let latest_generations = parse_last_xref_generations(&output);
    assert_eq!(latest_generations.get(&3), Some(&1));

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    assert_eq!(reopened.resolve(deleted_ref).unwrap(), Object::Null);

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(0, 65535)),
        Some(&XrefOffset::Free { next: 3 })
    );
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(3, 1)),
        Some(&XrefOffset::Free { next: 0 })
    );

    if is_qpdf_available() {
        let path =
            std::env::temp_dir().join(format!("flpdf-delete-object-{}.pdf", std::process::id()));
        fs::write(&path, &output).unwrap();
        let qpdf = Command::new("qpdf")
            .arg("--check")
            .arg(&path)
            .output()
            .unwrap_or_else(|err| panic!("failed to invoke qpdf: {err}"));
        let _ = fs::remove_file(&path);
        if qpdf_hit_empty_page_tree_bug(&qpdf) {
            eprintln!(
                "qpdf hit the empty-page-tree --check bug (flpdf-d4k); \
                 skipping the qpdf gate for this zero-page fixture"
            );
        } else {
            assert!(
                qpdf.status.success(),
                "qpdf failed: {}",
                String::from_utf8_lossy(&qpdf.stderr)
            );
        }
    }
}

#[test]
fn set_object_after_delete_keeps_object_live() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Old true >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let object_ref = ObjectRef::new(3, 0);
    let replacement = parse_object(b"<< /Replacement true >>").unwrap();
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    pdf.delete_object(object_ref);
    pdf.set_object(object_ref, replacement.clone());

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let latest_entries = parse_last_xref_entries(&output);
    assert_eq!(latest_entries.get(&3), Some(&b'n'));

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    assert_eq!(reopened.resolve(object_ref).unwrap(), replacement);
}

#[test]
fn delete_object_ignores_existing_free_tombstone() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n");
    bytes.extend_from_slice(b"0000000002 65535 f \n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", object_offsets[0]).as_bytes());
    bytes.extend_from_slice(b"0000000000 00001 f \n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", object_offsets[1]).as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    pdf.delete_object(ObjectRef::new(2, 1));

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 1)),
        Some(&XrefOffset::Free { next: 0 })
    );
}

#[test]
fn write_pdf_omits_unmapped_compressed_object_refs_from_xref() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let mut objects = Vec::new();
    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut objects,
    );

    let object_stream_body = b"2 0 7";
    add_object(
        format!(
            "3 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Length {} >>\nstream\n",
            object_stream_body.len()
        )
        .as_bytes(),
        &mut bytes,
        &mut objects,
    );
    bytes.extend_from_slice(object_stream_body);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[1] as u32, 0);

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();
    let xref = parse_last_xref_entries(&output);
    assert_eq!(xref.get(&0), Some(&b'f'));
    assert_eq!(xref.get(&1), Some(&b'n'));
    assert_eq!(xref.get(&3), Some(&b'n'));
    if matches!(loaded.last_xref_form, XrefForm::Stream) {
        assert_eq!(xref.get(&2), Some(&b'n'));
    } else {
        assert!(!xref.contains_key(&2));
    }
    assert_eq!(xref.get(&4), Some(&b'n'));
}

#[test]
fn write_pdf_incremental_trailer_strips_xref_stream_only_keys() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let mut objects = Vec::new();
    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut objects,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut objects,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, objects[1] as u32, 0);

    let xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, xref_offset as u32, 0);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&xref_entries).unwrap();
    let compressed_xref = encoder.finish().unwrap();

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} /Filter /FlateDecode /F 10 /FFilter /FlateDecode /FDecodeParms <<>> /XRefStm 123 >>\nstream\n",
            compressed_xref.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed_xref);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let trailer_section = parse_last_trailer_section(&output);
    assert!(!trailer_section.contains(" /Type /XRef"));
    assert!(!trailer_section.contains(" /W ["));
    assert!(!trailer_section.contains(" /Index ["));
    assert!(!trailer_section.contains(" /Length "));
    assert!(!trailer_section.contains(" /XRefStm "));
}

#[test]
fn write_pdf_rewrites_xref_stream_input_as_xref_stream_output() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
    let root_ref = pdf.root_ref().expect("expected /Root");
    let Object::Dictionary(mut root_dict) = pdf.resolve(root_ref).unwrap() else {
        panic!("root should be a dictionary");
    };
    root_dict.insert("FlpdfRegression", Object::Boolean(true));
    pdf.set_object(root_ref, Object::Dictionary(root_dict));

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(loaded.last_xref_form, XrefForm::Stream);
    let expected_prev = i64::try_from(source_xref_offset).unwrap();
    let actual_prev = as_integer(
        loaded
            .trailer
            .get("Prev")
            .expect("expected /Prev in xref stream"),
    );
    assert_eq!(actual_prev, Some(expected_prev));
    assert_eq!(&output[..source.len()], &source[..]);
}

#[test]
fn write_pdf_uses_non_colliding_xref_stream_object_number_for_new_objects() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 255);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    pdf.set_object(
        ObjectRef::new(4, 0),
        parse_object(b"<< /Generated true >>").unwrap(),
    );

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert!(loaded.entries.contains_key(&ObjectRef::new(4, 0)));
    assert!(loaded.entries.contains_key(&ObjectRef::new(5, 0)));
}

#[test]
fn write_pdf_preserves_xref_stream_trailer_metadata_and_declared_size() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"6 0 obj\n<< /Producer (flpdf-test) >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 255);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);
    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[2] as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 10 /Root 1 0 R /Info 6 0 R /ID [<0011> <2233>] /W [1 3 1] /Index [0 4 6 1] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(as_integer(loaded.trailer.get("Size").unwrap()), Some(10));
    assert_eq!(loaded.trailer.get_ref("Info"), Some(ObjectRef::new(6, 0)));
    assert!(loaded.trailer.get("ID").is_some());
}

#[test]
fn write_pdf_preserves_large_compressed_xref_stream_index() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"4 0 obj\n<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let large_index = 70_000;
    let mut xref_entries = Vec::new();
    append_xref_stream_entry_w4(&mut xref_entries, 0, 0, 65535);
    append_xref_stream_entry_w4(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry_w4(&mut xref_entries, 2, 4, large_index);
    append_xref_stream_entry_w4(&mut xref_entries, 1, object_offsets[1] as u32, 0);
    append_xref_stream_entry_w4(&mut xref_entries, 1, object_offsets[2] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry_w4(&mut xref_entries, 1, source_xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 4 4] /Index [0 6] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 0)),
        Some(&XrefOffset::Compressed {
            stream: 4,
            index: large_index
        })
    );
}

#[test]
fn write_pdf_preserves_xref_stream_free_tombstones() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Deleted false >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut first_xref_entries = Vec::new();
    append_xref_stream_entry(&mut first_xref_entries, 0, 0, 255);
    append_xref_stream_entry(&mut first_xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut first_xref_entries, 1, object_offsets[1] as u32, 0);
    append_xref_stream_entry(&mut first_xref_entries, 1, object_offsets[2] as u32, 0);

    let first_xref_offset = bytes.len();
    append_xref_stream_entry(&mut first_xref_entries, 1, first_xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
            first_xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&first_xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut tombstone_xref_entries = Vec::new();
    append_xref_stream_entry(&mut tombstone_xref_entries, 0, 0, 1);
    let tombstone_xref_offset = bytes.len();
    append_xref_stream_entry(
        &mut tombstone_xref_entries,
        1,
        tombstone_xref_offset as u32,
        0,
    );
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /Root 1 0 R /W [1 3 1] /Index [2 1 5 1] /Length {} /Prev {} >>\nstream\n",
            tombstone_xref_entries.len(),
            first_xref_offset,
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&tombstone_xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{tombstone_xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let latest_entries = parse_last_xref_entries(&output);
    assert_eq!(latest_entries.get(&2), Some(&b'f'));

    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.entries.get(&ObjectRef::new(2, 1)),
        Some(&XrefOffset::Free { next: 0 })
    );
}

#[test]
fn write_pdf_rewrites_flate_object_stream_member_and_recomputes_first() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let member_2_source =
        parse_object(format!("<< /Type /Packed /Payload ({}) >>", "A".repeat(400)).as_bytes())
            .unwrap();
    let member_2_rewritten =
        parse_object(format!("<< /Type /Packed /Payload ({}) >>", "A".repeat(401)).as_bytes())
            .unwrap();
    let member_3_source =
        parse_object(format!("<< /Type /Packed /Payload ({}) >>", "B".repeat(420)).as_bytes())
            .unwrap();

    let mut member2 = Vec::new();
    member_2_source.write_pdf(&mut member2);
    let mut member3 = Vec::new();
    member_3_source.write_pdf(&mut member3);
    let (stream_data, first) = build_flate_objstm_payload(&[(2, &member2[..]), (3, &member3[..])]);
    let obj_stream_offset = bytes.len();

    let obj_stream = format!(
        "4 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
        first,
        stream_data.len()
    )
    .into_bytes();
    bytes.extend_from_slice(&obj_stream);
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 1, obj_stream_offset as u32, 0);

    let xref_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.resolve(ObjectRef::new(2, 0)).unwrap(), member_2_source);
    pdf.set_object(ObjectRef::new(2, 0), member_2_rewritten.clone());

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    let Object::Stream(rewritten_obj_stream) = rewritten.resolve(ObjectRef::new(4, 0)).unwrap()
    else {
        panic!("expected object stream object");
    };

    let decoded =
        filters::decode_stream_data(&rewritten_obj_stream.dict, &rewritten_obj_stream.data)
            .unwrap();
    let parsed_first = as_integer(
        rewritten_obj_stream
            .dict
            .get("First")
            .expect("object stream missing /First"),
    )
    .expect("object stream /First should be integer");
    let parsed_first = usize::try_from(parsed_first).unwrap();
    assert!(
        parsed_first <= decoded.len(),
        "/First must fit in decoded stream"
    );
    assert_eq!(
        parsed_first,
        parse_objstm_first_header_len(&decoded, parsed_first).len()
    );

    let members = parse_objstm_members(&decoded, parsed_first);
    assert_eq!(members[0].0, 2);
    assert_eq!(members[1].0, 3);

    let first_member = parse_objstm_member(&decoded, parsed_first, &members, 0);
    let second_member = parse_objstm_member(&decoded, parsed_first, &members, 1);
    assert_eq!(first_member, member_2_rewritten);
    assert_eq!(second_member, member_3_source);
}

#[test]
fn write_pdf_rewrites_members_across_two_object_streams() {
    let member_2_source = parse_object(b"<< /Stream (first) >>").unwrap();
    let member_3_source = parse_object(b"<< /Stream (second) >>").unwrap();
    let member_5_source = parse_object(b"<< /Stream (third) >>").unwrap();
    let member_6_source = parse_object(b"<< /Stream (fourth) >>").unwrap();
    let member_2_rewritten = parse_object(b"<< /Stream (first updated) >>").unwrap();
    let member_5_rewritten = parse_object(b"<< /Stream (third updated) >>").unwrap();

    let source = two_flate_objstm_pdf(
        [&member_2_source, &member_3_source],
        [&member_5_source, &member_6_source],
    );
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    pdf.set_object(ObjectRef::new(2, 0), member_2_rewritten.clone());
    pdf.set_object(ObjectRef::new(5, 0), member_5_rewritten.clone());

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let latest_entries = parse_last_xref_entries(&output);
    assert_eq!(latest_entries.get(&4), Some(&b'n'));
    assert_eq!(latest_entries.get(&7), Some(&b'n'));

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    let (decoded_first_stream, first_stream_first) = decoded_objstm(&mut rewritten, 4);
    let first_stream_members = parse_objstm_members(&decoded_first_stream, first_stream_first);
    assert_eq!(
        parse_objstm_member(
            &decoded_first_stream,
            first_stream_first,
            &first_stream_members,
            0
        ),
        member_2_rewritten
    );
    assert_eq!(
        parse_objstm_member(
            &decoded_first_stream,
            first_stream_first,
            &first_stream_members,
            1
        ),
        member_3_source
    );

    let (decoded_second_stream, second_stream_first) = decoded_objstm(&mut rewritten, 7);
    let second_stream_members = parse_objstm_members(&decoded_second_stream, second_stream_first);
    assert_eq!(
        parse_objstm_member(
            &decoded_second_stream,
            second_stream_first,
            &second_stream_members,
            0
        ),
        member_5_rewritten
    );
    assert_eq!(
        parse_objstm_member(
            &decoded_second_stream,
            second_stream_first,
            &second_stream_members,
            1
        ),
        member_6_source
    );
}

#[test]
fn write_pdf_rewrites_two_members_in_one_object_stream() {
    let member_2_source = parse_object(b"<< /Name /One /Value 1 >>").unwrap();
    let member_3_source = parse_object(b"<< /Name /Two /Value 2 >>").unwrap();
    let member_4_source = parse_object(b"<< /Name /Three /Value 3 >>").unwrap();
    let member_2_rewritten = parse_object(b"<< /Name /One /Value 10 >>").unwrap();
    let member_4_rewritten = parse_object(b"<< /Name /Three /Value 30 >>").unwrap();
    let expected_untouched_bytes = rendered_object_bytes(&member_3_source);

    let source =
        three_member_flate_objstm_pdf([&member_2_source, &member_3_source, &member_4_source]);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    pdf.set_object(ObjectRef::new(2, 0), member_2_rewritten.clone());
    pdf.set_object(ObjectRef::new(4, 0), member_4_rewritten.clone());

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    assert_eq!(
        rewritten.resolve(ObjectRef::new(2, 0)).unwrap(),
        member_2_rewritten
    );
    assert_eq!(
        rewritten.resolve(ObjectRef::new(4, 0)).unwrap(),
        member_4_rewritten
    );
    let (decoded, first) = decoded_objstm(&mut rewritten, 5);
    let members = parse_objstm_members(&decoded, first);
    assert_eq!(
        parse_objstm_member(&decoded, first, &members, 1),
        member_3_source
    );
    assert_eq!(
        parse_objstm_member_bytes(&decoded, first, &members, 1),
        expected_untouched_bytes.as_slice()
    );
}

#[test]
fn write_pdf_preserves_reencoded_untouched_object_stream_member_bytes() {
    let member_2_source = parse_object(b"<< /Name /Updated /Value 1 >>").unwrap();
    let member_3_source = parse_object(b"<< /Name /Untouched /Array [1 2 3] >>").unwrap();
    let member_4_source = parse_object(b"<< /Name /AlsoUntouched /Value false >>").unwrap();
    let member_2_rewritten = parse_object(b"<< /Name /Updated /Value 2 >>").unwrap();
    let expected_member_3_bytes = rendered_object_bytes(&member_3_source);
    let expected_member_4_bytes = rendered_object_bytes(&member_4_source);

    let source =
        three_member_flate_objstm_pdf([&member_2_source, &member_3_source, &member_4_source]);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    pdf.set_object(ObjectRef::new(2, 0), member_2_rewritten.clone());

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    let (decoded, first) = decoded_objstm(&mut rewritten, 5);
    let members = parse_objstm_members(&decoded, first);
    assert_eq!(
        parse_objstm_member(&decoded, first, &members, 0),
        member_2_rewritten
    );
    assert_eq!(
        parse_objstm_member_bytes(&decoded, first, &members, 1),
        expected_member_3_bytes.as_slice()
    );
    assert_eq!(
        parse_objstm_member_bytes(&decoded, first, &members, 2),
        expected_member_4_bytes.as_slice()
    );
}

#[test]
fn write_pdf_rewrites_member_declared_in_extended_object_stream() {
    let source = objstm_extends_chain_pdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    assert_eq!(
        pdf.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(42)
    );
    pdf.set_object(ObjectRef::new(2, 0), Object::Integer(43));

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let output_text = String::from_utf8_lossy(&output);
    assert!(output_text.contains("/Extends 4 0 R"));

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    assert_eq!(
        rewritten.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(43)
    );
    assert_eq!(
        rewritten.resolve(ObjectRef::new(3, 0)).unwrap(),
        Object::Integer(99)
    );
}

#[test]
fn write_pdf_rewrites_unresolved_member_declared_in_extended_object_stream() {
    let source = objstm_extends_chain_pdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    pdf.set_object(ObjectRef::new(2, 0), Object::Integer(43));

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    assert_eq!(
        rewritten.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(43)
    );
    assert_eq!(
        rewritten.resolve(ObjectRef::new(3, 0)).unwrap(),
        Object::Integer(99)
    );
}

#[test]
fn write_pdf_preserves_extends_when_rewriting_extension_stream_member() {
    let source = objstm_extends_chain_pdf();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    pdf.set_object(ObjectRef::new(3, 0), Object::Integer(100));

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let mut rewritten = Pdf::open(Cursor::new(output)).unwrap();
    let Object::Stream(extension_stream) = rewritten.resolve(ObjectRef::new(5, 0)).unwrap() else {
        panic!("expected rewritten extension object stream");
    };
    assert_eq!(
        extension_stream.dict.get_ref("Extends"),
        Some(ObjectRef::new(4, 0))
    );
    assert_eq!(
        rewritten.resolve(ObjectRef::new(2, 0)).unwrap(),
        Object::Integer(42)
    );
    assert_eq!(
        rewritten.resolve(ObjectRef::new(3, 0)).unwrap(),
        Object::Integer(100)
    );
}

#[test]
fn write_pdf_rewrites_null_object_revision() {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    let mut object_offsets = Vec::new();
    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(b"2 0 obj\nnull\nendobj\n", &mut bytes, &mut object_offsets);

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n\n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            object_offsets.len() + 1,
        )
        .as_bytes(),
    );

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let null_object = pdf.resolve(ObjectRef::new(2, 0)).unwrap();
    assert_eq!(null_object, Object::Null);
    pdf.set_object(ObjectRef::new(2, 0), null_object);

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    assert_eq!(count_substrings(&output, b"2 0 obj"), 2);
}

#[test]
fn rewrites_pdf_with_real_numbers() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");

    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Contents 4 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", object_offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");

    for offset in object_offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n\n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<<\n  /Size {}\n  /Root 1 0 R\n>>\nstartxref\n{xref_offset}\n%%EOF\n",
            4 + 1,
        )
        .as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn rewrites_pdf_with_real_number_fixture() {
    let file = File::open("../../tests/fixtures/real-numbers-regression.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let page = pdf.resolve(ObjectRef::new(3, 0)).unwrap();
    let Object::Dictionary(page_dict) = page else {
        panic!("expected page dictionary")
    };
    assert_eq!(
        page_dict.get("MediaBox"),
        Some(&Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(1000.0),
            Object::Real(0.75),
        ]))
    );
    assert_eq!(
        page_dict.get("TrimBox"),
        Some(&Object::Array(vec![
            Object::Real(1.0),
            Object::Real(-0.25),
            Object::Real(0.25),
            Object::Real(-1.5),
        ]))
    );

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn rewrites_linearized_pdf_preserving_hint_object() {
    let input = linearized_fixture_pdf();
    let mut pdf = Pdf::open(Cursor::new(input)).unwrap();

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let output_text = String::from_utf8_lossy(&output);
    assert!(output_text.contains(" /Linearized "));
    assert!(output_text.contains("1 0 obj"));

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn rewrites_repaired_pdf_in_best_effort_mode() {
    let input = corrupt_xref_pdf();
    assert!(Pdf::open(Cursor::new(input.clone())).is_err());

    let mut pdf = Pdf::open_best_effort(Cursor::new(input)).unwrap();
    assert!(!pdf.repair_diagnostics().entries().is_empty());
    assert_eq!(pdf.version(), "1.7");

    let mut output = Vec::new();
    write_pdf(&mut pdf, &mut output).unwrap();

    let report = check_reader(Cursor::new(output)).unwrap();
    assert!(
        report.valid,
        "diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn write_qdf_header_has_binary_marker() {
    // Build a minimal PDF with version 1.5 so we can verify both the version
    // line and the binary marker line in the qdf output.
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // Line 1: version preserved from source
    assert!(
        output.starts_with(b"%PDF-1.5\n"),
        "expected %PDF-1.5 version line; got {:?}",
        std::str::from_utf8(&output[..output.len().min(20)]).unwrap_or("<invalid utf8>")
    );

    // Line 2: binary marker must be exactly qpdf's 6-byte sequence
    // `%` + 4 high bytes (0xBF 0xF7 0xA2 0xFE) + newline
    let expected_marker: &[u8] = b"%\xbf\xf7\xa2\xfe\n";
    assert_eq!(
        &output[b"%PDF-1.5\n".len()..b"%PDF-1.5\n".len() + expected_marker.len()],
        expected_marker,
        "binary marker bytes mismatch; got {:02x?}",
        &output[b"%PDF-1.5\n".len()..b"%PDF-1.5\n".len() + expected_marker.len()]
    );
}

/// A full rewrite normalizes every object's generation to 0 and drops objects
/// unreachable from `/Root`. This fixture's `/Root` is `1 3 R` (a non-zero
/// input generation — the sole mixed-generation exercise of the Catalog-first
/// renumber map) and object 3 is an orphan, so after the qdf full rewrite the
/// catalog must reappear at generation 0 and the orphan must be gone. (qpdf
/// behaves the same way: full rewrite renumbers everything to `1..=N` gen 0.)
#[test]
fn writes_qdf_normalizes_object_generations() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 3 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Orphan >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", 4).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    bytes.extend_from_slice(format!("{:010} 00003 n\n", offsets[0]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n\n", offsets[1]).as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n\n", offsets[2]).as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 3 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // Re-open and verify the normalization contract via references, not
    // hardcoded numbers.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let root = reopened.root_ref().expect("output has /Root");
    assert_eq!(
        root.generation, 0,
        "full rewrite must normalize the /Root generation to 0 (was 3)"
    );
    match reopened.resolve(root).expect("resolve /Root") {
        Object::Dictionary(d) => assert_eq!(
            d.get("Type"),
            Some(&Object::Name(b"Catalog".to_vec())),
            "the generation-3 catalog content must survive the renumber"
        ),
        other => panic!("/Root must be a dictionary, got {other:?}"),
    }

    // The orphan (obj 3, unreachable from /Root) must be dropped.
    let rendered = String::from_utf8_lossy(&output);
    assert!(
        !rendered.contains("/Orphan"),
        "object unreachable from /Root must be dropped by the full rewrite"
    );
}

#[test]
fn write_qdf_goes_through_canonical_qdf_serializers() {
    // flpdf-9hc.24: write_qdf() is now a thin wrapper over the canonical QDF
    // path (write_pdf_with_options { qdf: true, full_rewrite: true }), so its
    // output must carry the canonical QDF markers built by epic flpdf-9hc.6
    // — NOT the old compact dump. (`WriteOptions::no_original_object_ids`
    // still defaults to false; the canonical path emits the comment unless
    // that flag is set.)
    let opts = WriteOptions::default();
    assert!(
        !opts.no_original_object_ids,
        "no_original_object_ids must default to false (default behavior unchanged)"
    );

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let off1 = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();
    let rendered = String::from_utf8_lossy(&output);

    assert!(
        rendered.contains("%QDF-1.0"),
        "canonical write_qdf must emit the %QDF-1.0 header marker"
    );
    assert!(
        rendered.contains("%% Original object ID:"),
        "canonical write_qdf must emit %% Original object ID: comments"
    );
    assert!(
        rendered.contains("\ntrailer <<"),
        "canonical write_qdf must use the `trailer <<` dict layout"
    );
    assert!(
        rendered.contains("\nxref\n"),
        "canonical write_qdf must use a classic xref table"
    );
    assert!(
        !rendered.contains("/Type /XRef"),
        "canonical write_qdf must not emit a cross-reference stream"
    );
    assert!(
        !rendered.contains("/Type /ObjStm"),
        "canonical write_qdf must not emit object streams"
    );
}

/// QDF output must carry qpdf's page-context comments — one `%% Page N` before
/// each Page dict and one `%% Contents for page N` before each content stream —
/// keyed by 1-based page order. Mirrors qpdf 11.9.0 QPDFWriter.cc:1774-1785.
/// These markers are separate from `%% Original object ID:` and MUST remain
/// even when `no_original_object_ids = true`.
#[test]
fn write_qdf_emits_page_and_contents_markers_per_page() {
    let file = File::open("../../tests/fixtures/compat/three-page.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // One `%% Page N` marker per page (three-page fixture has 3 pages).
    for n in 1..=3 {
        let needle = format!("%% Page {n}\n");
        assert!(
            output.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "write_qdf must emit `%% Page {n}` marker before the Page dict"
        );
    }

    // One `%% Contents for page N` marker per page (three-page.pdf pages
    // each carry a single indirect /Contents stream).
    for n in 1..=3 {
        let needle = format!("%% Contents for page {n}\n");
        assert!(
            output.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "write_qdf must emit `%% Contents for page {n}` marker before the content stream"
        );
    }
}

/// Even under `no_original_object_ids = true` (qpdf's `--no-original-object-ids`),
/// the QDF page/contents markers must still be emitted — qpdf keeps them
/// regardless of that flag; only `%% Original object ID:` is suppressed.
#[test]
fn qdf_no_original_object_ids_still_emits_page_and_contents_markers() {
    let file = File::open("../../tests/fixtures/compat/three-page.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let mut opts = WriteOptions::default();
    opts.full_rewrite = true;
    opts.qdf = true;
    opts.no_original_object_ids = true;
    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &opts).unwrap();

    // %% Original object ID: must be suppressed.
    assert!(
        !output
            .windows(b"%% Original object ID:".len())
            .any(|w| w == b"%% Original object ID:"),
        "no_original_object_ids must suppress `%% Original object ID:` comments"
    );

    // But page/contents markers must remain (one per page, 3 pages).
    for n in 1..=3 {
        let page_needle = format!("%% Page {n}\n");
        assert!(
            output
                .windows(page_needle.len())
                .any(|w| w == page_needle.as_bytes()),
            "no_original_object_ids must NOT suppress `%% Page {n}`"
        );
        let contents_needle = format!("%% Contents for page {n}\n");
        assert!(
            output
                .windows(contents_needle.len())
                .any(|w| w == contents_needle.as_bytes()),
            "no_original_object_ids must NOT suppress `%% Contents for page {n}`"
        );
    }
}

/// Pages without a `/Contents` entry (legitimately blank pages) must not
/// panic or skip the QDF page-marker emission. This exercises the
/// `contents_raw = None` arm of the QDF page/contents pre-scan: `%% Page N`
/// is still emitted for every page, but `%% Contents for page N` is only
/// emitted for pages that actually have a content stream.
#[test]
fn write_qdf_handles_page_without_contents_key() {
    // A minimal 1.4 PDF with one Page dict that has NO /Contents key.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let off1 = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n");
    let off3 = bytes.len();
    // Page dict without /Contents — a blank page.
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // %% Page 1 must still be emitted (page seq is independent of /Contents).
    assert!(
        output
            .windows(b"%% Page 1\n".len())
            .any(|w| w == b"%% Page 1\n"),
        "%% Page 1 must be emitted even when the page has no /Contents"
    );
    // %% Contents for page 1 must NOT be emitted — no content stream exists.
    assert!(
        !output
            .windows(b"%% Contents for page 1".len())
            .any(|w| w == b"%% Contents for page 1"),
        "%% Contents for page N must not be emitted when the page has no /Contents"
    );
}

/// A page whose `/Contents` is an INDIRECT reference resolving to an ARRAY
/// of stream refs must tag each array element with the page's sequence
/// number in the QDF pre-scan (`%% Contents for page N` before every content
/// stream). Exercises the `Indirect → resolve → Array` arm of the pre-scan.
#[test]
fn write_qdf_handles_indirect_contents_ref_resolving_to_array_of_refs() {
    // Minimal 1.4 PDF: Page's /Contents is `4 0 R`; object 4 is an ARRAY
    // holding two content-stream refs `5 0 R` and `6 0 R`.
    let stream_a = b"BT /F1 12 Tf ET";
    let stream_b = b"0 0 m 10 10 l S";
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let off1 = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n");
    let off3 = bytes.len();
    // /Contents references object 4 (which is an Array).
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = bytes.len();
    bytes.extend_from_slice(b"4 0 obj\n[5 0 R 6 0 R]\nendobj\n");
    let off5 = bytes.len();
    bytes.extend_from_slice(
        format!("5 0 obj\n<< /Length {} >>\nstream\n", stream_a.len()).as_bytes(),
    );
    bytes.extend_from_slice(stream_a);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let off6 = bytes.len();
    bytes.extend_from_slice(
        format!("6 0 obj\n<< /Length {} >>\nstream\n", stream_b.len()).as_bytes(),
    );
    bytes.extend_from_slice(stream_b);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for off in [off1, off2, off3, off4, off5, off6] {
        bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // qpdf's pre-scan tags EVERY element of the Array with the page's seq,
    // so we expect TWO `%% Contents for page 1` comments (one before each
    // stream object). Count them.
    let needle = b"%% Contents for page 1\n";
    let count = output
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count();
    assert_eq!(
        count, 2,
        "QDF pre-scan must tag both array-element streams with `%% Contents for page 1` (found {count})"
    );
}

/// A page whose `/Contents` is a DIRECT Array containing a mix of Reference
/// and non-Reference elements (e.g. a stray `null`) must still tag the
/// Reference elements. Exercises the `_ => None` filter_map arm that skips
/// non-Reference direct-Array elements.
#[test]
fn write_qdf_direct_contents_array_skips_non_reference_elements() {
    // Minimal 1.4 PDF where /Contents is `[ 5 0 R null ]` (mixed direct
    // array). Only the Reference element must appear in the pre-scan
    // tagging.
    let stream_a = b"BT /F1 12 Tf ET";
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let off1 = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n");
    let off3 = bytes.len();
    bytes.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [5 0 R null] >>\nendobj\n",
    );
    let off5 = bytes.len();
    bytes.extend_from_slice(
        format!("5 0 obj\n<< /Length {} >>\nstream\n", stream_a.len()).as_bytes(),
    );
    bytes.extend_from_slice(stream_a);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
    // Object 4 is unused / free.
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );

    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // Exactly one `%% Contents for page 1` — from the Reference element
    // `5 0 R`. The `null` element is skipped by the filter_map.
    let needle = b"%% Contents for page 1\n";
    let count = output
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count();
    assert_eq!(
        count, 1,
        "QDF pre-scan must tag exactly one content stream when the direct \
         /Contents Array mixes a Reference with a non-Reference (found {count})"
    );
}

/// QDF stream dict serialization must pull `/Length` to the last position
/// (right before `>>`), matching qpdf's non-QDF `/Length`-last convention
/// applied in the multi-line QDF layout. A stream dict with alphabetically-
/// sorted keys must show `/Length` after every other key.
#[test]
fn write_qdf_stream_dict_pulls_length_to_end() {
    let file = File::open("../../tests/fixtures/compat/three-page.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
    let mut output = Vec::new();
    write_qdf(&mut pdf, &mut output).unwrap();

    // Every occurrence of `/Length ` in a stream dict must be immediately
    // followed by an indirect ref (`N 0 R`) and then `\n` + optional spaces +
    // `>>` — the QDF stream dict pulls /Length last. Search for the pattern
    // `/Length N 0 R\n` followed by any leading whitespace and `>>`.
    let mut found_length_last = false;
    let text = String::from_utf8_lossy(&output);
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("/Length ") && trimmed.ends_with(" 0 R") {
            // The next non-blank line must be `>>` (dict close).
            let next = text.lines().nth(i + 1).unwrap_or("").trim();
            if next == ">>" {
                found_length_last = true;
                break;
            }
        }
    }
    assert!(
        found_length_last,
        "write_qdf must emit `/Length N 0 R` as the last dict key immediately \
         before `>>` in at least one stream dict"
    );
}

fn count_substrings(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }

    let mut count = 0;
    let mut start = 0;
    while let Some(position) = haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        count += 1;
        start += position + needle.len();
    }

    count
}

fn is_qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// qpdf 12.x aborts `qpdf --check` with a C++ `std::out_of_range` thrown from
/// `std::vector::at()` whenever the document's page tree is empty
/// (`/Type /Pages` with `/Count 0` and no `/Kids`) — an upstream qpdf bug
/// (flpdf-d4k; cf. qpdf issue #31). libstdc++ renders the message as
/// `vector::_M_range_check: ...`, libc++ (macOS) as a bare `vector`; both
/// surface on stderr as a line starting `ERROR: vector`. flpdf's zero-page
/// fixtures are valid PDFs (qpdf 11.x and the spec accept them), so when qpdf
/// hits this bug it cannot serve as an oracle — callers skip the `--check`
/// gate rather than fail it. Self-heals once qpdf fixes the bug upstream.
fn qpdf_hit_empty_page_tree_bug(qpdf: &std::process::Output) -> bool {
    // Anchored to a line *starting* `ERROR: vector` (not an unanchored
    // substring) so a real qpdf error that merely mentions the word is not
    // mistaken for the upstream crash.
    !qpdf.status.success()
        && String::from_utf8_lossy(&qpdf.stderr)
            .lines()
            .any(|line| line.trim_end().starts_with("ERROR: vector"))
}

fn parse_startxref(bytes: &[u8]) -> u64 {
    let marker = b"startxref";
    let eof = bytes
        .windows(b"%%EOF".len())
        .rposition(|window| window == b"%%EOF")
        .unwrap_or(bytes.len());
    let search = &bytes[..eof];

    let Some(pos) = search
        .windows(marker.len())
        .rposition(|window| window == marker)
    else {
        panic!("missing startxref marker")
    };

    let mut cursor = pos + marker.len();
    while cursor < search.len() && search[cursor].is_ascii_whitespace() {
        cursor += 1;
    }

    let start = cursor;
    while cursor < search.len() && search[cursor].is_ascii_digit() {
        cursor += 1;
    }

    if start == cursor {
        panic!("missing startxref offset")
    }

    let value = std::str::from_utf8(&search[start..cursor]).unwrap();
    value.parse::<u64>().unwrap()
}

fn parse_last_xref_generations(bytes: &[u8]) -> BTreeMap<u32, u16> {
    let rendered = String::from_utf8_lossy(bytes);
    let xref_pos = if let Some(pos) = rendered.rfind("\nxref\n") {
        pos
    } else if let Some(pos) = rendered.rfind("xref\n") {
        pos.saturating_sub(1)
    } else {
        panic!("missing xref section");
    };

    let section_pos = if &rendered[xref_pos + 1..xref_pos + 6] == "xref\n" {
        xref_pos + 1
    } else {
        panic!("missing xref section");
    };

    let mut lines = rendered[section_pos + 5..].lines();
    let mut generations = BTreeMap::new();

    while let Some(section) = lines.next() {
        // The trailer keyword now carries its dict on the same line
        // (`trailer << ... >>`), so match the prefix, not the bare word.
        if section.starts_with("trailer") {
            break;
        }

        let mut fields = section.split_whitespace();
        let start: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("invalid xref subsection header: {section}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref start token {section}"));
        let count: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("missing xref count for start {start}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref count token {section}"));

        for index in 0..count {
            let entry_line = lines
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let mut entry_fields = entry_line.split_whitespace();

            let _ = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let generation: u16 = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing generation in xref subsection {start} {count}"))
                .parse()
                .unwrap_or_else(|_| {
                    panic!("invalid generation in xref subsection {start} {count}")
                });

            generations.insert(start + index, generation);
        }
    }

    generations
}

fn parse_last_trailer_section(bytes: &[u8]) -> String {
    let rendered = String::from_utf8_lossy(bytes);
    let trailer_pos = rendered
        .rfind("trailer")
        .unwrap_or_else(|| panic!("missing trailer"));
    let trailer_start = rendered[trailer_pos..]
        .find("<<")
        .map(|offset| trailer_pos + offset)
        .unwrap_or_else(|| panic!("missing trailer dictionary"));
    let startxref_pos = rendered[trailer_pos..]
        .find("startxref")
        .map(|offset| trailer_pos + offset)
        .unwrap_or_else(|| panic!("missing startxref"));

    rendered[trailer_start..startxref_pos].to_string()
}

fn as_integer(object: &Object) -> Option<i64> {
    match object {
        Object::Integer(value) => Some(*value),
        _ => None,
    }
}

fn parse_last_xref_entries(bytes: &[u8]) -> BTreeMap<u32, u8> {
    let startxref = usize::try_from(parse_startxref(bytes)).unwrap();
    if bytes[startxref..].starts_with(b"xref") {
        return parse_last_xref_table_entries(bytes, startxref);
    }

    let mut cursor = Cursor::new(bytes);
    let loaded = load_xref_and_trailer(&mut cursor)
        .expect("output should include a valid xref stream dictionary");
    let stream_data = latest_xref_stream_data(bytes, startxref);
    let (w0, w1, w2) = xref_stream_widths(&loaded.trailer);
    let ranges = xref_stream_ranges(&loaded.trailer);
    let entry_width = w0 + w1 + w2;
    let mut entries = BTreeMap::new();

    let mut pos = 0;
    for (start, count) in ranges {
        for index in 0..count {
            let entry = &stream_data[pos..pos + entry_width];
            let object_type = if w0 == 0 {
                1
            } else {
                read_be_usize(&entry[..w0])
            };

            entries.insert(
                start + index,
                match object_type {
                    0 => b'f',
                    1 | 2 => b'n',
                    other => panic!("unsupported xref stream entry type {other}"),
                },
            );
            pos += entry_width;
        }
    }

    entries
}

fn parse_last_xref_table_entries(bytes: &[u8], startxref: usize) -> BTreeMap<u32, u8> {
    let rendered = String::from_utf8_lossy(&bytes[startxref..]);
    let mut lines = rendered[5..].lines();
    let mut entries = BTreeMap::new();

    while let Some(section) = lines.next() {
        // The trailer keyword now carries its dict on the same line
        // (`trailer << ... >>`), so match the prefix, not the bare word.
        if section.starts_with("trailer") {
            break;
        }

        let mut fields = section.split_whitespace();
        let start: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("invalid xref subsection header: {section}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref start token {section}"));
        let count: u32 = fields
            .next()
            .unwrap_or_else(|| panic!("missing xref count for start {start}"))
            .parse()
            .unwrap_or_else(|_| panic!("invalid xref count token {section}"));

        for index in 0..count {
            let entry_line = lines
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let mut entry_fields = entry_line.split_whitespace();
            let _ = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing offset in xref subsection {start} {count}"));
            let _ = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing generation in xref subsection {start} {count}"));
            let status = entry_fields
                .next()
                .unwrap_or_else(|| panic!("missing status in xref subsection {start} {count}"));

            entries.insert(start + index, status.as_bytes()[0]);
        }
    }

    entries
}

fn latest_xref_stream_data(bytes: &[u8], startxref: usize) -> &[u8] {
    let tail = &bytes[startxref..];
    let stream_marker = b"stream";
    let stream_pos = tail
        .windows(stream_marker.len())
        .position(|window| window == stream_marker)
        .unwrap_or_else(|| panic!("missing xref stream data"));
    let mut data_start = startxref + stream_pos + stream_marker.len();
    if bytes.get(data_start) == Some(&b'\r') {
        data_start += 1;
    }
    if bytes.get(data_start) == Some(&b'\n') {
        data_start += 1;
    }
    let end_marker = b"endstream";
    let data_end = bytes[data_start..]
        .windows(end_marker.len())
        .position(|window| window == end_marker)
        .map(|offset| data_start + offset)
        .unwrap_or_else(|| panic!("missing xref endstream"));
    let mut data_end = data_end;
    while data_end > data_start && bytes[data_end - 1].is_ascii_whitespace() {
        data_end -= 1;
    }

    &bytes[data_start..data_end]
}

fn xref_stream_widths(trailer: &flpdf::Dictionary) -> (usize, usize, usize) {
    let Some(Object::Array(widths)) = trailer.get("W") else {
        panic!("xref stream missing /W")
    };
    assert_eq!(widths.len(), 3);
    (
        as_integer(&widths[0]).unwrap() as usize,
        as_integer(&widths[1]).unwrap() as usize,
        as_integer(&widths[2]).unwrap() as usize,
    )
}

fn xref_stream_ranges(trailer: &flpdf::Dictionary) -> Vec<(u32, u32)> {
    if let Some(Object::Array(index)) = trailer.get("Index") {
        return index
            .chunks_exact(2)
            .map(|chunk| {
                (
                    as_integer(&chunk[0]).unwrap() as u32,
                    as_integer(&chunk[1]).unwrap() as u32,
                )
            })
            .collect();
    }

    let size = as_integer(trailer.get("Size").expect("xref stream missing /Size")).unwrap() as u32;
    vec![(0, size)]
}

fn read_be_usize(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0usize, |value, byte| (value << 8) | usize::from(*byte))
}

fn append_u24_be(bytes: &mut Vec<u8>, value: u32) {
    let bytes_u24 = value.to_be_bytes();
    bytes.extend_from_slice(&bytes_u24[1..]);
}

fn build_flate_objstm_payload(members: &[(u32, &[u8])]) -> (Vec<u8>, usize) {
    let mut header = String::new();
    let mut body = Vec::new();

    for (index, (number, object_data)) in members.iter().enumerate() {
        let offset = body.len();
        header.push_str(&format!("{} {} ", number, offset));
        body.extend_from_slice(object_data);
        if index + 1 < members.len() {
            body.push(b'\n');
        }
    }

    let mut decoded = Vec::new();
    decoded.extend_from_slice(header.as_bytes());
    decoded.extend_from_slice(&body);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&decoded).unwrap();
    let encoded = encoder.finish().unwrap();

    (encoded, header.len())
}

fn two_flate_objstm_pdf(first_members: [&Object; 2], second_members: [&Object; 2]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let offsets = [bytes.len()];
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let first_member_bytes: Vec<Vec<u8>> = first_members
        .iter()
        .map(|object| rendered_object_bytes(object))
        .collect();
    let second_member_bytes: Vec<Vec<u8>> = second_members
        .iter()
        .map(|object| rendered_object_bytes(object))
        .collect();
    let (first_stream_data, first_first) = build_flate_objstm_payload(&[
        (2, first_member_bytes[0].as_slice()),
        (3, first_member_bytes[1].as_slice()),
    ]);
    let first_stream_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
            first_first,
            first_stream_data.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&first_stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let (second_stream_data, second_first) = build_flate_objstm_payload(&[
        (5, second_member_bytes[0].as_slice()),
        (6, second_member_bytes[1].as_slice()),
    ]);
    let second_stream_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "7 0 obj\n<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
            second_first,
            second_stream_data.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&second_stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 4, 1);
    append_xref_stream_entry(&mut xref_entries, 1, first_stream_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 7, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 7, 1);
    append_xref_stream_entry(&mut xref_entries, 1, second_stream_offset as u32, 0);

    let xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "8 0 obj\n<< /Type /XRef /Size 9 /Root 1 0 R /W [1 3 1] /Index [0 9] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
}

fn three_member_flate_objstm_pdf(members: [&Object; 3]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let root_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");

    let member_bytes: Vec<Vec<u8>> = members
        .iter()
        .map(|object| rendered_object_bytes(object))
        .collect();
    let (stream_data, first) = build_flate_objstm_payload(&[
        (2, member_bytes[0].as_slice()),
        (3, member_bytes[1].as_slice()),
        (4, member_bytes[2].as_slice()),
    ]);
    let objstm_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /ObjStm /N 3 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
            first,
            stream_data.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&stream_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, root_offset as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 5, 0);
    append_xref_stream_entry(&mut xref_entries, 2, 5, 1);
    append_xref_stream_entry(&mut xref_entries, 2, 5, 2);
    append_xref_stream_entry(&mut xref_entries, 1, objstm_offset as u32, 0);

    let xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R /W [1 3 1] /Index [0 7] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    bytes
}

fn decoded_objstm(pdf: &mut Pdf<Cursor<Vec<u8>>>, object_number: u32) -> (Vec<u8>, usize) {
    let Object::Stream(object_stream) = pdf.resolve(ObjectRef::new(object_number, 0)).unwrap()
    else {
        panic!("expected object stream object");
    };
    let decoded = filters::decode_stream_data(&object_stream.dict, &object_stream.data).unwrap();
    let first = as_integer(
        object_stream
            .dict
            .get("First")
            .expect("object stream missing /First"),
    )
    .expect("object stream /First should be integer");
    (decoded, usize::try_from(first).unwrap())
}

fn rendered_object_bytes(object: &Object) -> Vec<u8> {
    let mut bytes = Vec::new();
    object.write_pdf(&mut bytes);
    bytes
}

fn parse_objstm_first_header_len(decoded: &[u8], declared_first: usize) -> &[u8] {
    &decoded[..declared_first]
}

fn parse_objstm_members(decoded: &[u8], first: usize) -> Vec<(u32, usize)> {
    let header = std::str::from_utf8(&decoded[..first]).unwrap();
    let tokens: Vec<&str> = header.split_whitespace().map(str::trim).collect();
    let mut members = Vec::new();

    for pair in tokens.chunks(2) {
        assert_eq!(pair.len(), 2);
        let number = pair[0].parse().unwrap();
        let offset = pair[1].parse().unwrap();
        members.push((number, offset));
    }

    members
}

fn parse_objstm_member(
    decoded: &[u8],
    first: usize,
    members: &[(u32, usize)],
    index: usize,
) -> Object {
    let (number, start) = members[index];
    let start = first + start;
    let end = if index + 1 < members.len() {
        first + members[index + 1].1
    } else {
        decoded.len()
    };
    let _ = number;
    parse_object(trim_right_ws(&decoded[start..end])).unwrap()
}

fn parse_objstm_member_bytes<'a>(
    decoded: &'a [u8],
    first: usize,
    members: &[(u32, usize)],
    index: usize,
) -> &'a [u8] {
    let (_number, start) = members[index];
    let start = first + start;
    let end = if index + 1 < members.len() {
        first + members[index + 1].1
    } else {
        decoded.len()
    };
    trim_right_ws(&decoded[start..end])
}

fn trim_right_ws(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |idx| idx + 1);
    &bytes[..end]
}

fn append_xref_stream_entry(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u8) {
    entries.push(entry_type);
    append_u24_be(entries, field1);
    entries.push(field2);
}

fn append_xref_stream_entry_w4(entries: &mut Vec<u8>, entry_type: u8, field1: u32, field2: u32) {
    entries.push(entry_type);
    entries.extend_from_slice(&field1.to_be_bytes());
    entries.extend_from_slice(&field2.to_be_bytes());
}

fn objstm_extends_chain_pdf() -> Vec<u8> {
    decode_hex_fixture(include_str!(
        "../../../tests/fixtures/compat/objstm-extends-chain.pdf.hex"
    ))
}

fn decode_hex_fixture(hex: &str) -> Vec<u8> {
    let digits: Vec<u8> = hex
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    assert!(digits.len().is_multiple_of(2));

    digits
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}

fn linearized_fixture_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Linearized 1 /L 100 /E 0 /N 1 /T 1 >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"3 0 obj\n<< /Type /Pages /Count 1 /Kids [4 0 R] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 595.28 841.89] /Contents 5 0 R >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );
    add_object(
        b"5 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    let object_count = offsets.len() + 1;
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 2 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            object_count
        )
        .as_bytes(),
    );

    bytes
}

fn corrupt_xref_pdf() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec();
    let obj2 = b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n".to_vec();
    let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>\nendobj\n".to_vec();
    let obj4 = b"4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\n".to_vec();

    let mut offsets = Vec::new();
    for object in &[obj1, obj2, obj3, obj4] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }

    let start_xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f\n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start_xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );

    let mut corrupted = bytes;
    let Some(pos) = corrupted.windows(4).position(|window| window == b"xref") else {
        unreachable!("fixture should contain xref token")
    };
    if let Some(byte) = corrupted.get_mut(pos + 2) {
        *byte = b'z';
    }
    corrupted
}

// ---------------------------------------------------------------------------
// Full-rewrite mode tests
// ---------------------------------------------------------------------------

/// Build a minimal PDF in-memory whose stream uses a single FlateDecode filter.
fn build_minimal_pdf_with_stream(stream_data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder as Z;
    use flate2::Compression as C;
    let mut enc = Z::new(Vec::new(), C::default());
    enc.write_all(stream_data).unwrap();
    let compressed = enc.finish().unwrap();

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // object 1: catalog. References the stream (obj 3) via /Metadata so it
    // stays reachable from /Root and survives the Catalog-first reachability
    // walk (which drops objects unreachable from /Root).
    offsets.push(bytes.len());
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");
    // object 2: pages
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    // object 3: content stream with FlateDecode
    offsets.push(bytes.len());
    let stream_header = format!(
        "3 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
        compressed.len()
    );
    bytes.extend_from_slice(stream_header.as_bytes());
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

/// Build a minimal PDF whose stream uses a multi-filter chain: [ASCII85Decode, FlateDecode].
/// This is the kind of output produced by ReportLab.
///
/// The PDF decode pipeline for [ASCII85Decode, FlateDecode] is:
///   stored → ASCII85 decode → FlateDecode decode → raw
/// so stored = ASCII85(FlateDecode(raw)).
fn build_pdf_with_multi_filter_stream() -> Vec<u8> {
    let raw_data = b"Hello from multi-filter stream!".repeat(20);
    // Build stored = ASCII85(Flate(raw)):
    //   Step 1: FlateDecode encode raw → flated
    //   Step 2: ASCII85Decode encode flated → a85
    let mut flate_dict = Dictionary::new();
    flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    let flated = filters::encode_stream_data(&flate_dict, &raw_data).unwrap();

    let mut a85_dict = Dictionary::new();
    a85_dict.insert("Filter", Object::Name(b"ASCII85Decode".to_vec()));
    let a85 = filters::encode_stream_data(&a85_dict, &flated).unwrap();

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // object 1: catalog. References the stream (obj 3) via /Metadata so it
    // stays reachable from /Root and survives the Catalog-first reachability
    // walk (which drops objects unreachable from /Root).
    offsets.push(bytes.len());
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");
    // object 2: pages
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    // object 3: content stream with [ASCII85Decode FlateDecode]
    offsets.push(bytes.len());
    let stream_header = format!(
        "3 0 obj\n<< /Filter [/ASCII85Decode /FlateDecode] /Length {} >>\nstream\n",
        a85.len()
    );
    bytes.extend_from_slice(stream_header.as_bytes());
    bytes.extend_from_slice(&a85);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn full_rewrite_minimal_pdf_is_valid() {
    let source = build_minimal_pdf_with_stream(b"content data");
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "full-rewrite output should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn full_rewrite_no_prev_in_trailer() {
    let source = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Trailer must not contain /Prev.
    let reopened = Pdf::open(Cursor::new(&output)).unwrap();
    assert!(
        reopened.trailer().get("Prev").is_none(),
        "full-rewrite output must not have /Prev in trailer"
    );
}

#[test]
fn full_rewrite_single_flatedecode_filter() {
    let source = build_minimal_pdf_with_stream(b"stream payload data for filter check");
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Re-open and inspect stream 3's filter.
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let metadata_ref = metadata_stream_ref(&mut reopened);
    let stream_obj = reopened.resolve(metadata_ref).unwrap();
    let Object::Stream(stream) = stream_obj else {
        panic!("/Metadata must resolve to a stream");
    };
    match stream.dict.get("Filter") {
        Some(Object::Name(name)) => {
            assert_eq!(
                name.as_slice(),
                b"FlateDecode",
                "stream Filter should be FlateDecode"
            );
        }
        Some(Object::Array(_)) => {
            // Also acceptable if the filter is a single-element array [/FlateDecode]
            // but our implementation uses a Name directly.
        }
        other => panic!("unexpected Filter: {:?}", other),
    }
    // No DecodeParms from the old filter chain.
    // Verify the stream decodes to the original data.
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data).unwrap();
    assert_eq!(decoded, b"stream payload data for filter check");
}

#[test]
fn full_rewrite_multi_filter_decodes_and_reencodes() {
    // Input has [ASCII85Decode FlateDecode] — multi-filter chain from ReportLab.
    let source = build_pdf_with_multi_filter_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "full-rewrite of multi-filter PDF should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // Stream 3 should now have a single FlateDecode filter (not ASCII85+Flate).
    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let metadata_ref = metadata_stream_ref(&mut reopened);
    let stream_obj = reopened.resolve(metadata_ref).unwrap();
    let Object::Stream(stream) = stream_obj else {
        panic!("/Metadata must resolve to a stream");
    };
    // Filter must NOT be an array containing ASCII85Decode.
    match stream.dict.get("Filter") {
        Some(Object::Array(filters_arr)) => {
            for f in filters_arr {
                if let Object::Name(name) = f {
                    assert_ne!(
                        name.as_slice(),
                        b"ASCII85Decode",
                        "ASCII85Decode must be removed by full-rewrite"
                    );
                }
            }
        }
        Some(Object::Name(name)) => {
            assert_ne!(
                name.as_slice(),
                b"ASCII85Decode",
                "ASCII85Decode must be removed by full-rewrite"
            );
        }
        None => {} // uncompressed is also fine
        other => panic!("unexpected Filter: {:?}", other),
    }

    // Verify round-trip: decode the output stream and compare to original raw data.
    let raw_data = b"Hello from multi-filter stream!".repeat(20);
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data).unwrap();
    assert_eq!(
        decoded, raw_data,
        "full-rewrite stream round-trip should recover original data"
    );
}

#[test]
fn full_rewrite_default_options_still_incremental() {
    // With default WriteOptions (full_rewrite=false), source bytes are preserved.
    let source_bytes = std::fs::read("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(Cursor::new(source_bytes.clone())).unwrap();

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &WriteOptions::default()).unwrap();

    // Incremental: source bytes are a prefix of the output.
    assert!(
        output.starts_with(&source_bytes),
        "incremental write should preserve source bytes as a prefix"
    );
}

#[test]
fn full_rewrite_from_fixture() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut pdf = Pdf::open(BufReader::new(file)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "full-rewrite of fixture should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // Verify that the output contains the PDF header.
    let header = &output[..9];
    assert!(
        header.starts_with(b"%PDF-"),
        "output should start with PDF header"
    );
}

/// Build a minimal PDF that uses an xref stream (instead of an xref table).
/// The xref stream has /W [1 3 1] entries and no /Filter (uncompressed).
fn build_minimal_pdf_with_xref_stream() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // object 1: catalog
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    // object 2: pages
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let xref_offset = bytes.len();

    // Build raw xref stream bytes: W=[1,3,1]
    // Entry for obj 0: type=0 (free), field1=0, field2=0
    // Entry for obj 1: type=1 (in-use), field1=offset, field2=0 (generation)
    // Entry for obj 2: type=1 (in-use), field1=offset, field2=0
    // Entry for obj 3 (xref stream itself): type=1, field1=xref_offset, field2=0
    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[1] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

#[test]
fn full_rewrite_xref_stream_input_produces_valid_pdf() {
    // Verify that full_rewrite handles a PDF whose source uses xref stream form
    // (XrefForm::Stream) — the XrefForm::Stream branch of write_pdf_full_rewrite.
    let source = build_minimal_pdf_with_xref_stream();
    // Confirm the fixture itself uses xref stream form before testing full_rewrite.
    {
        let mut reader = Cursor::new(&source);
        let loaded = load_xref_and_trailer(&mut reader).unwrap();
        assert_eq!(
            loaded.last_xref_form,
            XrefForm::Stream,
            "fixture must use xref stream form"
        );
    }

    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "full-rewrite of xref-stream input should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

#[test]
fn full_rewrite_xref_stream_input_no_prev() {
    // /Prev must be absent in the full-rewrite output even when the source used
    // xref stream form.
    let source = build_minimal_pdf_with_xref_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();
    assert!(
        loaded.trailer.get("Prev").is_none(),
        "full-rewrite output from xref-stream input must not have /Prev"
    );
}

#[test]
fn full_rewrite_xref_stream_input_downgrades_to_classic_table_under_force_version() {
    // qpdf treats a forced sub-1.5 header as a hard cap it will not exceed.
    // Cross-reference streams are a PDF 1.5 feature, so qpdf does NOT clamp the
    // header up to 1.5 to keep the inherited stream form — it keeps the forced
    // 1.4 header and rebuilds a CLASSIC xref table. flpdf matches that (prime
    // directive: qpdf over spec purity). This is a degenerate hand-built fixture
    // qpdf cannot process, so the check is structural; byte-parity vs a real
    // qpdf golden lives in the cmp_* suites.
    let source = build_minimal_pdf_with_xref_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.force_version = Some("1.4".to_string());

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(
        output.starts_with(b"%PDF-1.4\n"),
        "forced sub-1.5 header must be kept (not clamped to 1.5); got: {:?}",
        std::str::from_utf8(&output[..16.min(output.len())]).unwrap_or("<invalid utf-8>")
    );
    assert!(
        !String::from_utf8_lossy(&output).contains("/Type /XRef"),
        "inherited xref stream must be downgraded to a classic xref table under force<1.5"
    );
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "downgraded output must be a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );
}

/// Build a tiny xref-stream PDF whose xref stream declares
/// `/Filter /FlateDecode` and stores FlateDecode-encoded entries. Used to
/// verify that `write_pdf_full_rewrite` does not propagate stale filter
/// declarations from the source trailer into the rebuilt xref stream.
fn build_minimal_pdf_with_flate_xref_stream() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let xref_offset = bytes.len();

    let mut raw_entries = Vec::new();
    append_xref_stream_entry(&mut raw_entries, 0, 0, 0);
    append_xref_stream_entry(&mut raw_entries, 1, offsets[0] as u32, 0);
    append_xref_stream_entry(&mut raw_entries, 1, offsets[1] as u32, 0);
    append_xref_stream_entry(&mut raw_entries, 1, xref_offset as u32, 0);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw_entries).unwrap();
    let encoded = encoder.finish().unwrap();

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Filter /FlateDecode /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            encoded.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&encoded);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

#[test]
fn full_rewrite_xref_stream_compress_yes_produces_valid_flate_xref() {
    // Regression / policy test: when the source PDF's xref stream declares
    // `/Filter /FlateDecode` the full-rewrite path used to inherit that key
    // from `pdf.trailer().clone()` while emitting **raw** entry bytes,
    // producing an unreadable PDF.  Now `CompressStreams::Yes` (the default)
    // properly FlateDecode-compresses the rebuilt xref bytes and sets
    // `/Filter /FlateDecode` deliberately.  Verify the output parses cleanly.
    let source = build_minimal_pdf_with_flate_xref_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    // compress_streams defaults to CompressStreams::Yes

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // The output must parse back cleanly (FlateDecode xref stream is valid).
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "full-rewrite output from filtered xref-stream input should be valid; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // With CompressStreams::Yes the rebuilt xref stream carries /Filter
    // /FlateDecode (set deliberately by the writer, not inherited stale).
    let mut reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();
    assert_eq!(
        loaded.trailer.get("Filter"),
        Some(&Object::Name(b"FlateDecode".to_vec())),
        "CompressStreams::Yes xref stream must declare /Filter /FlateDecode"
    );
    // Keys that must never appear (only stale external-file refs / decode parms).
    for key in ["DecodeParms", "F", "FFilter", "FDecodeParms"] {
        assert!(
            loaded.trailer.get(key).is_none(),
            "rebuilt xref stream must not carry /{key}"
        );
    }
}

#[test]
fn full_rewrite_xref_stream_compress_no_strips_all_filter_keys() {
    // With CompressStreams::No the rebuilt xref bytes are stored raw and no
    // /Filter key should appear in the xref stream dictionary.
    let source = build_minimal_pdf_with_flate_xref_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.compress_streams = CompressStreams::No;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // The output must parse back cleanly.
    let report = check_reader(Cursor::new(&output)).unwrap();
    assert!(
        report.valid,
        "CompressStreams::No full-rewrite should produce a valid PDF; diagnostics: {:?}",
        report.diagnostics.entries()
    );

    // No filter keys in the xref stream dict.
    let mut reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();
    for key in ["Filter", "DecodeParms", "F", "FFilter", "FDecodeParms"] {
        assert!(
            loaded.trailer.get(key).is_none(),
            "CompressStreams::No xref stream must not carry /{key}"
        );
    }
}

/// Build a minimal PDF whose content stream declares `/F` to point at an
/// external file in addition to its embedded data.  Used to verify that
/// `reencode_stream_flate` strips `/F` so readers don't load the stale
/// external reference instead of the newly produced embedded Flate stream.
fn build_pdf_with_external_file_stream() -> Vec<u8> {
    use flate2::write::ZlibEncoder as Z;
    use flate2::Compression as C;
    let payload = b"embedded stream payload for external-file regression";
    let mut enc = Z::new(Vec::new(), C::default());
    enc.write_all(payload).unwrap();
    let compressed = enc.finish().unwrap();

    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    // The Catalog references the stream (obj 3) via /Metadata so it stays
    // reachable from /Root and survives the Catalog-first reachability walk.
    offsets.push(bytes.len());
    bytes
        .extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 3 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");
    offsets.push(bytes.len());
    let stream_header = format!(
        "3 0 obj\n<< /Filter /FlateDecode /F (external.bin) /Length {} >>\nstream\n",
        compressed.len()
    );
    bytes.extend_from_slice(stream_header.as_bytes());
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn full_rewrite_strips_external_file_ref_from_reencoded_stream() {
    // Regression: previously /F survived through reencode_stream_flate, so a
    // re-emitted Flate stream still pointed at the (now-stale) external file
    // and readers honoring /F would ignore the embedded payload.
    let source = build_pdf_with_external_file_stream();
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let metadata_ref = metadata_stream_ref(&mut reopened);
    let stream_obj = reopened.resolve(metadata_ref).unwrap();
    let Object::Stream(stream) = stream_obj else {
        panic!("/Metadata must resolve to a stream");
    };
    for key in ["F", "FFilter", "FDecodeParms"] {
        assert!(
            stream.dict.get(key).is_none(),
            "re-encoded stream must not carry /{key}"
        );
    }
}

// ---------------------------------------------------------------------------
// PDF header version setters (--min-version / --force-version) — flpdf-9hc.13.1
//
// These tests pin the full-rewrite write path (the path a plain `flpdf
// rewrite` takes). They assert the *header bytes*, not merely that the write
// succeeds, and they assert that `/Catalog /Version` is preserved verbatim
// (qpdf's observed semantics: `qpdf --force-version`/`--min-version` rewrites
// only the `%PDF-x.y` line and never touches the Catalog's `/Version` entry).
// ---------------------------------------------------------------------------

/// Build a tiny classic-xref-table PDF with the given header version string
/// and an optional `/Catalog /Version` name entry (e.g. `Some("1.7")`).
fn build_pdf_with_optional_catalog_version(header: &str, catalog_version: Option<&str>) -> Vec<u8> {
    let mut bytes = format!("%PDF-{header}\n").into_bytes();
    let mut offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    let catalog = match catalog_version {
        Some(v) => format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Version /{v} >>\nendobj\n"),
        None => "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
    };
    add_object(catalog.as_bytes(), &mut bytes, &mut offsets);
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut offsets,
    );

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1,
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn full_rewrite_force_version_sets_header_exactly() {
    let source = build_pdf_with_optional_catalog_version("1.7", None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.force_version = Some("1.4".to_string());

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(
        output.starts_with(b"%PDF-1.4\n"),
        "force_version must set header to 1.4 even when source is higher; got {:?}",
        std::str::from_utf8(&output[..9]).unwrap_or("<bad>")
    );
}

#[test]
fn full_rewrite_min_version_raises_header_when_source_lower() {
    let source = build_pdf_with_optional_catalog_version("1.3", None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.min_version = Some("1.7".to_string());

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(
        output.starts_with(b"%PDF-1.7\n"),
        "min_version must raise header 1.3 -> 1.7; got {:?}",
        std::str::from_utf8(&output[..9]).unwrap_or("<bad>")
    );
}

#[test]
fn full_rewrite_min_version_is_noop_when_source_already_higher() {
    let source = build_pdf_with_optional_catalog_version("1.7", None);
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.min_version = Some("1.3".to_string());

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(
        output.starts_with(b"%PDF-1.7\n"),
        "min_version below source must be a no-op (header stays 1.7); got {:?}",
        std::str::from_utf8(&output[..9]).unwrap_or("<bad>")
    );
}

#[test]
fn full_rewrite_preserves_catalog_version_verbatim_under_force_version() {
    // qpdf semantics (verified empirically against qpdf 11.x):
    // `qpdf --force-version=1.4` rewrites only the `%PDF-x.y` line and leaves
    // the Catalog's `/Version` entry untouched, even when it is *higher* than
    // the chosen header. "Reconciled per qpdf semantics" therefore means
    // "leave /Catalog /Version alone" — readers compute the effective version
    // as max(header, catalog), but the writer must not strip or lower it.
    let source = build_pdf_with_optional_catalog_version("1.3", Some("1.7"));
    let mut pdf = Pdf::open(Cursor::new(source)).unwrap();

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.force_version = Some("1.4".to_string());

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    assert!(
        output.starts_with(b"%PDF-1.4\n"),
        "header must be forced to 1.4; got {:?}",
        std::str::from_utf8(&output[..9]).unwrap_or("<bad>")
    );

    let mut reopened = Pdf::open(Cursor::new(&output)).unwrap();
    let Object::Dictionary(catalog) = reopened.resolve(ObjectRef::new(1, 0)).unwrap() else {
        panic!("object 1 should be the Catalog dictionary");
    };
    match catalog.get("Version") {
        Some(Object::Name(name)) => assert_eq!(
            name.as_slice(),
            b"1.7",
            "/Catalog /Version must be preserved verbatim (qpdf does not touch it)"
        ),
        other => panic!("expected /Catalog /Version /1.7 to be preserved, got {other:?}"),
    }
}

/// flpdf-9hc.5.9 Task 5: incremental write of an xref-stream source with
/// `ObjectStreamMode::Generate` packs a mutated plain eligible object into a
/// freshly-allocated ObjStm container appended in the incremental update.
///
/// All assertions go through the public reader API
/// (`Pdf::open`/`resolve` and `load_xref_and_trailer`); no hand parsing of
/// xref bytes. The plan's `pdf.compressed_parent(...)` check is expressed via
/// `LoadedXref::entries` (`XrefOffset::Compressed`) because
/// `Pdf::compressed_parent` / `Pdf::previous_xref_offset` are `pub(crate)`
/// and not reachable from the integration-test crate.
#[test]
fn incremental_generate_roundtrip_packs_mutated_object_into_objstm() {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    {
        let mut src_reader = Cursor::new(&source);
        let src_loaded = load_xref_and_trailer(&mut src_reader).unwrap();
        assert_eq!(
            src_loaded.last_xref_form,
            XrefForm::Stream,
            "fixture must be xref-stream form for the Generate gate to engage"
        );
    }
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();

    // Mutate object 2 — a plain, gen-0, non-stream, /Type /Pages dict, which is
    // eligible for ObjStm packing (only /ObjStm and /XRef types are blocked).
    let mutated_ref = ObjectRef::new(2, 0);
    let mutated_value = Object::Dictionary({
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(b"Pages".to_vec()));
        d.insert("Count", Object::Integer(0));
        d.insert("Kids", Object::Array(Vec::new()));
        d.insert("FlpdfMutated", Object::Boolean(true));
        d
    });
    pdf.set_object(mutated_ref, mutated_value.clone());

    let mut options = WriteOptions::default();
    options.object_streams = ObjectStreamMode::Generate;
    options.full_rewrite = false;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // Container number is whatever `allocate_incremental_objstm_container`
    // computes: max(max_source=3, max_touched=2, max_deleted=0,
    // declared_size-1=3) + 1 == 4.
    let expected_container = ObjectRef::new(4, 0);

    // (a)+(b): re-open and resolve via the reader API.
    let mut reopened = Pdf::open(Cursor::new(output.clone())).unwrap();
    assert_eq!(
        reopened.resolve(mutated_ref).unwrap(),
        mutated_value,
        "(a) mutated object must resolve to the new value"
    );
    let root_ref = reopened.root_ref().expect("expected /Root");
    let Object::Dictionary(catalog) = reopened.resolve(root_ref).unwrap() else {
        panic!("(b) /Root must still resolve to a dictionary");
    };
    assert_eq!(
        catalog.get("Type"),
        Some(&Object::Name(b"Catalog".to_vec())),
        "(b) untouched /Catalog must still resolve"
    );

    // (c): mutated object is now compressed into the new container at index 0,
    // and the container resolves to an /ObjStm stream.
    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.last_xref_form,
        XrefForm::Stream,
        "output must remain xref-stream form"
    );
    match loaded.entries.get(&mutated_ref) {
        Some(XrefOffset::Compressed { stream, index }) => {
            assert_eq!(
                *stream, expected_container.number,
                "(c) mutated object must be compressed into the new container"
            );
            assert_eq!(*index, 0, "(c) mutated object must be at ObjStm index 0");
        }
        other => panic!(
            "(c) mutated object {mutated_ref:?} must have a compressed xref entry, got {other:?}"
        ),
    }
    let Object::Stream(container_stream) = reopened.resolve(expected_container).unwrap() else {
        panic!("(c) container {expected_container:?} must resolve to a stream");
    };
    assert_eq!(
        container_stream.dict.get("Type"),
        Some(&Object::Name(b"ObjStm".to_vec())),
        "(c) container /Type must be /ObjStm"
    );

    // (d): trailer /Size covers the new container number.
    let size = as_integer(
        loaded
            .trailer
            .get("Size")
            .expect("(d) appended xref stream must declare /Size"),
    )
    .expect("(d) /Size must be an integer");
    assert!(
        size > i64::from(expected_container.number),
        "(d) trailer /Size ({size}) must be >= container number + 1 ({})",
        expected_container.number + 1
    );

    // (e): /Prev points at the source's last startxref.
    let prev = as_integer(
        loaded
            .trailer
            .get("Prev")
            .expect("(e) appended xref stream must carry /Prev"),
    )
    .expect("(e) /Prev must be an integer");
    assert_eq!(
        prev,
        i64::try_from(source_xref_offset).unwrap(),
        "(e) /Prev must equal the source's last startxref"
    );
}

/// Structural-correctness gate: a generate-mode incremental update (mutated
/// plain object packed into a freshly-allocated ObjStm, appended over an
/// xref-stream source with full_rewrite=false) must produce a PDF that qpdf
/// accepts.
///
/// `qpdf --check` validates the whole framing: xref offsets, the /Prev chain,
/// container as a type-1 object, the compressed member as a type-2 entry, and
/// trailer /Size. `qpdf --show-object=2` additionally proves qpdf walked the
/// /Prev chain into the new container, decompressed the mutated object, and
/// rendered the *updated* value (the `FlpdfMutated` marker).
///
/// Gated on qpdf availability via the established `is_qpdf_available()` helper;
/// when qpdf is absent the structural gate is not exercised.
#[test]
fn incremental_generate_qpdf_check() {
    if !is_qpdf_available() {
        eprintln!("qpdf not available; skipping incremental_generate_qpdf_check structural gate");
        return;
    }

    // Fixture replicates `incremental_generate_roundtrip_packs_mutated_object_into_objstm`
    // (inline, per this file's convention — no shared helper extraction).
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut object_offsets = Vec::new();

    let add_object = |object: &[u8], bytes: &mut Vec<u8>, offsets: &mut Vec<usize>| {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    };

    add_object(
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );
    add_object(
        b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n",
        &mut bytes,
        &mut object_offsets,
    );

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, object_offsets[1] as u32, 0);

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0);

    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /XRef /Size 4 /Root 1 0 R /W [1 3 1] /Index [0 4] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();

    let mutated_ref = ObjectRef::new(2, 0);
    let mutated_value = Object::Dictionary({
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(b"Pages".to_vec()));
        d.insert("Count", Object::Integer(0));
        d.insert("Kids", Object::Array(Vec::new()));
        d.insert("FlpdfMutated", Object::Boolean(true));
        d
    });
    pdf.set_object(mutated_ref, mutated_value);

    let mut options = WriteOptions::default();
    options.object_streams = ObjectStreamMode::Generate;
    options.full_rewrite = false;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    let path = std::env::temp_dir().join(format!(
        "flpdf-incremental-generate-qpdf-{}.pdf",
        std::process::id()
    ));
    fs::write(&path, &output).unwrap();

    // (1) Structural validation: `qpdf --check` must accept the framing.
    let check = Command::new("qpdf")
        .arg("--check")
        .arg(&path)
        .output()
        .unwrap_or_else(|err| panic!("failed to invoke qpdf --check: {err}"));

    // (2) The mutated object must be resolvable through the /Prev chain into
    // the new ObjStm container, rendering the *updated* value.
    let show = Command::new("qpdf")
        .arg("--show-object=2")
        .arg(&path)
        .output()
        .unwrap_or_else(|err| panic!("failed to invoke qpdf --show-object: {err}"));

    let _ = fs::remove_file(&path);

    if qpdf_hit_empty_page_tree_bug(&check) {
        eprintln!(
            "qpdf hit the empty-page-tree --check bug (flpdf-d4k); \
             skipping the qpdf --check gate for this zero-page fixture"
        );
    } else {
        assert!(
            check.status.success(),
            "qpdf --check failed on incremental generate output:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&check.stdout),
            String::from_utf8_lossy(&check.stderr)
        );
    }

    assert!(
        show.status.success(),
        "qpdf --show-object=2 failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&show.stdout),
        String::from_utf8_lossy(&show.stderr)
    );
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_out.contains("FlpdfMutated"),
        "qpdf --show-object=2 must render the mutated object's updated value \
         (expected /FlpdfMutated marker); got:\n{show_out}"
    );
}

/// flpdf-9hc.5.12: combined-paths regression. A single incremental Generate
/// write over an xref-stream source must, in ONE pass, satisfy all three
/// flows independently:
///
/// - (a) a freshly-mutated plain eligible object is packed into a NEW ObjStm
///   container (type-2 compressed entry with `stream` == new container);
/// - (b) a deleted object emits a type-0 free entry;
/// - (c) a touched existing-ObjStm member is patched IN PLACE — its xref
///   entry stays `Compressed{ stream: original_container, index: original_index }`.
///
/// The flpdf-9hc.5.9 stack already covers each path in isolation, plus a
/// qpdf-check on the packable-only path; the combined-paths interaction is
/// currently satisfied only transitively (the gate routes plain-packable
/// through `partition_objstm_eligible` while leaving `deleted_object_refs`
/// and `touched_objstm_members` on their pre-existing flows). This
/// integration test hardens against a future refactor accidentally
/// re-entangling those flows — e.g. routing an existing-ObjStm-member touch
/// through the new packer (which would flip its `stream` from the source
/// container to the new container and would silently re-pack rather than
/// patch in place).
#[test]
fn incremental_generate_combined_paths_packs_deletes_and_touches_existing_member() {
    // Build an xref-stream-form source with:
    //   1 0 obj   /Catalog
    //   2 0 obj   /Pages (compressed in ObjStm container 3, index 0)
    //   3 0 obj   ObjStm  (Flate, single member)
    //   4 0 obj   plain   (to be deleted in the incremental update)
    //   5 0 obj   plain   (to be touched → packed into NEW ObjStm)
    //   6 0 obj   xref stream
    //
    // /Catalog references object 2 via /Pages so the document is structurally
    // walkable from /Root through the ObjStm container; objects 4 and 5 are
    // unreferenced from /Catalog, which qpdf --check tolerates as warnings.
    let mut bytes = b"%PDF-1.7\n".to_vec();

    let obj1_offset = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // ObjStm container 3 holding member object 2 (Pages dict).
    let member2_source = b"<< /Type /Pages /Count 0 /Kids [] /FlpdfMemberSource true >>";
    let (objstm_data, objstm_first) = build_flate_objstm_payload(&[(2, member2_source)]);
    let obj3_offset = bytes.len();
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
            objstm_first,
            objstm_data.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&objstm_data);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let obj4_offset = bytes.len();
    bytes.extend_from_slice(b"4 0 obj\n<< /FlpdfTag (deletable) >>\nendobj\n");

    let obj5_offset = bytes.len();
    bytes.extend_from_slice(b"5 0 obj\n<< /FlpdfTag (packable-source) >>\nendobj\n");

    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0); // 0: free head
    append_xref_stream_entry(&mut xref_entries, 1, obj1_offset as u32, 0); // 1: type-1
    append_xref_stream_entry(&mut xref_entries, 2, 3, 0); // 2: type-2 (in stream 3, idx 0)
    append_xref_stream_entry(&mut xref_entries, 1, obj3_offset as u32, 0); // 3: ObjStm container
    append_xref_stream_entry(&mut xref_entries, 1, obj4_offset as u32, 0); // 4: type-1
    append_xref_stream_entry(&mut xref_entries, 1, obj5_offset as u32, 0); // 5: type-1

    let source_xref_offset = bytes.len();
    append_xref_stream_entry(&mut xref_entries, 1, source_xref_offset as u32, 0); // 6: xref stream

    bytes.extend_from_slice(
        format!(
            "6 0 obj\n<< /Type /XRef /Size 7 /Root 1 0 R /W [1 3 1] /Index [0 7] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{source_xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    {
        let mut src_reader = Cursor::new(&source);
        let src_loaded = load_xref_and_trailer(&mut src_reader).unwrap();
        assert_eq!(
            src_loaded.last_xref_form,
            XrefForm::Stream,
            "fixture must be xref-stream form for the Generate gate to engage"
        );
        // Confirm the fixture sets up object 2 as an existing ObjStm member —
        // this is the load-bearing precondition of path (c); without it
        // `set_object(2,…)` would not populate `compressed_member_parents`
        // and the writer would route obj2 through the plain-touched path.
        match src_loaded.entries.get(&ObjectRef::new(2, 0)) {
            Some(XrefOffset::Compressed { stream, index }) => {
                assert_eq!(*stream, 3, "fixture: obj 2 must be in source ObjStm 3");
                assert_eq!(*index, 0, "fixture: obj 2 must be at source ObjStm index 0");
            }
            other => {
                panic!("fixture must register obj 2 as Compressed in source xref; got {other:?}")
            }
        }
    }

    let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();

    // (c) Rewrite the existing ObjStm member — same /Pages-shaped dict, new
    // tag to make the round-trip distinguishable.
    let member_ref = ObjectRef::new(2, 0);
    let member_rewritten = Object::Dictionary({
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(b"Pages".to_vec()));
        d.insert("Count", Object::Integer(0));
        d.insert("Kids", Object::Array(Vec::new()));
        d.insert("FlpdfMemberRewritten", Object::Boolean(true));
        d
    });
    pdf.set_object(member_ref, member_rewritten.clone());

    // (b) Delete object 4 → must emit a type-0 free entry.
    let deleted_ref = ObjectRef::new(4, 0);
    pdf.delete_object(deleted_ref);

    // (a) Touch object 5 with a plain eligible dict → packed into NEW ObjStm.
    let packed_ref = ObjectRef::new(5, 0);
    let packed_value = Object::Dictionary({
        let mut d = Dictionary::new();
        d.insert("FlpdfTag", Object::String(b"packable-rewritten".to_vec()));
        d.insert("FlpdfPacked", Object::Boolean(true));
        d
    });
    pdf.set_object(packed_ref, packed_value.clone());

    let mut options = WriteOptions::default();
    options.object_streams = ObjectStreamMode::Generate;
    options.full_rewrite = false;

    let mut output = Vec::new();
    write_pdf_with_options(&mut pdf, &mut output, &options).unwrap();

    // New container number per `allocate_incremental_objstm_container`:
    //   max(max_source=6, max_touched=5, max_deleted=4, declared_size-1=6) + 1 = 7.
    let expected_new_container = ObjectRef::new(7, 0);

    // Round-trip resolutions through the public reader API.
    let mut reopened = Pdf::open(Cursor::new(output.clone())).unwrap();
    assert_eq!(
        reopened.resolve(member_ref).unwrap(),
        member_rewritten,
        "(c) touched existing-ObjStm member must resolve to the rewritten value"
    );
    assert_eq!(
        reopened.resolve(packed_ref).unwrap(),
        packed_value,
        "(a) packed object must resolve to the rewritten value"
    );
    assert_eq!(
        reopened.resolve(deleted_ref).unwrap(),
        Object::Null,
        "(b) deleted object must resolve to /Null"
    );
    // /Catalog and the source ObjStm container must still be reachable.
    let root_ref = reopened.root_ref().expect("expected /Root");
    let Object::Dictionary(catalog) = reopened.resolve(root_ref).unwrap() else {
        panic!("/Root must resolve to a dictionary");
    };
    assert_eq!(
        catalog.get("Type"),
        Some(&Object::Name(b"Catalog".to_vec())),
        "/Catalog must still resolve"
    );

    // Inspect the appended xref stream directly so each path's *xref entry
    // kind* is asserted, not just its resolution result.
    let mut output_reader = Cursor::new(&output);
    let loaded = load_xref_and_trailer(&mut output_reader).unwrap();
    assert_eq!(
        loaded.last_xref_form,
        XrefForm::Stream,
        "output must remain xref-stream form"
    );

    // (a) packed → type-2 compressed entry in the NEW container at index 0.
    match loaded.entries.get(&packed_ref) {
        Some(XrefOffset::Compressed { stream, index }) => {
            assert_eq!(
                *stream, expected_new_container.number,
                "(a) packed object must be compressed into the freshly-allocated container"
            );
            assert_eq!(
                *index, 0,
                "(a) packed object must be at new-container index 0"
            );
        }
        other => panic!(
            "(a) packed object {packed_ref:?} must have a Compressed xref entry, got {other:?}"
        ),
    }
    // New container itself must be a plain type-1 indirect object.
    match loaded.entries.get(&expected_new_container) {
        Some(XrefOffset::Offset(_)) => {}
        other => panic!(
            "(a) new ObjStm container {expected_new_container:?} must have a type-1 Offset entry, got {other:?}"
        ),
    }
    // And the new container's body must actually be an /ObjStm stream.
    let Object::Stream(container_stream) = reopened.resolve(expected_new_container).unwrap() else {
        panic!("(a) new container {expected_new_container:?} must resolve to a stream");
    };
    assert_eq!(
        container_stream.dict.get("Type"),
        Some(&Object::Name(b"ObjStm".to_vec())),
        "(a) new container /Type must be /ObjStm"
    );

    // (b) deleted → type-0 free entry. `build_deleted_entries` bumps the
    // generation by 1 (incremented_generation), so the new free entry lives at
    // ObjectRef{ number: 4, generation: 1 } in the appended xref stream.
    let deleted_free_key = ObjectRef::new(deleted_ref.number, 1);
    match loaded.entries.get(&deleted_free_key) {
        Some(XrefOffset::Free { .. }) => {}
        other => panic!(
            "(b) deleted object {deleted_ref:?} must produce a Free xref entry at gen 1, got {other:?}"
        ),
    }

    // (c) existing-ObjStm member — IN-PLACE patch. The defining property of
    // this regression test: `stream` must remain the SOURCE container (3),
    // not flip to the freshly-allocated one. A future refactor that routes
    // existing-member touches through the new packer would flip `stream` to
    // 7 and trip this assertion.
    let source_container = ObjectRef::new(3, 0);
    match loaded.entries.get(&member_ref) {
        Some(XrefOffset::Compressed { stream, index }) => {
            assert_eq!(
                *stream, source_container.number,
                "(c) touched existing-ObjStm member must stay in its source container — \
                 if this fires with stream={} (the new container), the existing-member \
                 path has been re-entangled with the new packer",
                expected_new_container.number
            );
            assert_eq!(
                *index, 0,
                "(c) touched existing-ObjStm member must retain its source index"
            );
        }
        other => panic!(
            "(c) touched existing-ObjStm member {member_ref:?} must have a Compressed xref entry, got {other:?}"
        ),
    }
    // The source container must have been rewritten at a NEW offset (the
    // patched payload reflects the touched member); its xref kind stays
    // type-1 Offset.
    match loaded.entries.get(&source_container) {
        Some(XrefOffset::Offset(_)) => {}
        other => panic!(
            "(c) source ObjStm container {source_container:?} must remain type-1 Offset after patch, got {other:?}"
        ),
    }
    // And the patched container must actually contain the rewritten member.
    let Object::Stream(patched) = reopened.resolve(source_container).unwrap() else {
        panic!("(c) source container {source_container:?} must still resolve to a stream");
    };
    assert_eq!(
        patched.dict.get("Type"),
        Some(&Object::Name(b"ObjStm".to_vec())),
        "(c) source container /Type must stay /ObjStm"
    );

    // /Prev must point at the source's last startxref (incremental linkage).
    let prev = as_integer(
        loaded
            .trailer
            .get("Prev")
            .expect("appended xref stream must carry /Prev"),
    )
    .expect("/Prev must be an integer");
    assert_eq!(
        prev,
        i64::try_from(source_xref_offset).unwrap(),
        "/Prev must equal the source's last startxref"
    );

    // qpdf --check structural gate (skipped if qpdf is unavailable).
    if is_qpdf_available() {
        let path = std::env::temp_dir().join(format!(
            "flpdf-incremental-generate-combined-{}.pdf",
            std::process::id()
        ));
        fs::write(&path, &output).unwrap();
        let check = Command::new("qpdf")
            .arg("--check")
            .arg(&path)
            .output()
            .unwrap_or_else(|err| panic!("failed to invoke qpdf --check: {err}"));
        let _ = fs::remove_file(&path);
        if qpdf_hit_empty_page_tree_bug(&check) {
            eprintln!(
                "qpdf hit the empty-page-tree --check bug (flpdf-d4k); \
                 skipping the qpdf --check gate for this zero-page fixture"
            );
        } else {
            assert!(
                check.status.success(),
                "qpdf --check failed on combined-paths incremental generate output:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&check.stdout),
                String::from_utf8_lossy(&check.stderr)
            );
        }
    }
}

// ── flpdf-9hc.5.9 Task 7: fallback regression for the incremental generate gate
//
// The Task-5 gate (writer.rs) is exactly:
//
//   use_objstm = (object_streams == Generate)
//             && (last_xref_form == Stream)
//             && (partition_objstm_eligible yields a NON-EMPTY packable)
//
// When ANY condition is false the incremental path is byte-identical to the
// pre-Task-5 plain incremental write: `plain_touched == touched_object_refs`
// and `objstm_inc` stays `None`. The three tests below each disable EXACTLY
// ONE gate condition and assert the full output `Vec<u8>` is byte-identical
// to the legacy (gate-not-engaging) write of the SAME source + SAME mutation.
//
// Because line 384 of writer.rs is the ONLY incremental-path reader of
// `options.object_streams`, a leak — any code outside the gate that started
// branching on `Generate` — would make the two sides diverge and fail these
// asserts. The fixtures are built so the gate predicate IS exercised on every
// run (table form, or mode toggled, or ineligible-only mutation), which is
// what makes each assert_eq! non-tautological. Test 2 additionally proves the
// gate genuinely ENGAGES when all three conditions hold (assert_ne!), so the
// "does not engage in Preserve/Disable" equalities are not vacuous.

/// Build a minimal xref-TABLE PDF (classic `xref`/`trailer`/`startxref`)
/// carrying a plain, gen-0, non-stream /Type /Pages object (eligible for
/// ObjStm packing). Used by test 1: the gate's `XrefForm::Stream` condition
/// is false for a table source, so `use_objstm` is false regardless of mode.
fn build_minimal_pdf_with_xref_table() -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let startxref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

/// Test 1 — gate condition disabled: **`XrefForm::Stream`** (source is a
/// classic xref TABLE).
///
/// Two sides, identical source + identical mutation, differing ONLY in
/// `object_streams`:
///   - side A (legacy baseline): `Preserve` (the pre-Task-5 default)
///   - side B (feature path):    `Generate`
///
/// Because the source uses an xref TABLE, `matches!(last_xref_form, Stream)`
/// is false on BOTH sides, so `use_objstm` is false and both take the plain
/// incremental path. The outputs MUST be byte-identical: the Generate request
/// must not perturb the write when the xref-form condition gates it out.
///
/// Non-tautological: the two calls differ by `object_streams` (Generate vs
/// Preserve) — the exact axis the gate keys on. If any logic outside the
/// gate branched on `Generate`, side B would differ and this assert_eq! would
/// fail. The mutated object is genuinely ObjStm-eligible (gen-0, non-stream,
/// /Type /Pages), so only the xref-form condition keeps the gate shut — the
/// predicate is really exercised.
#[test]
fn incremental_generate_fallback_table_source_is_byte_identical() {
    let source = build_minimal_pdf_with_xref_table();
    {
        let mut r = Cursor::new(&source);
        let loaded = load_xref_and_trailer(&mut r).unwrap();
        assert_eq!(
            loaded.last_xref_form,
            XrefForm::Table,
            "fixture must be xref-TABLE form so the Generate gate's stream condition is false"
        );
    }

    let mutated_ref = ObjectRef::new(2, 0);
    let mutated_value = Object::Dictionary({
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(b"Pages".to_vec()));
        d.insert("Count", Object::Integer(0));
        d.insert("Kids", Object::Array(Vec::new()));
        d.insert("FlpdfMutated", Object::Boolean(true));
        d
    });

    // Side A — legacy baseline: default (Preserve) mode, plain incremental.
    let mut pdf_a = Pdf::open(Cursor::new(source.clone())).unwrap();
    pdf_a.set_object(mutated_ref, mutated_value.clone());
    let mut opts_a = WriteOptions::default();
    opts_a.object_streams = ObjectStreamMode::Preserve;
    opts_a.full_rewrite = false;
    // `static_id` makes the trailer /ID deterministic so the only thing the
    // assert can detect is a gate-driven divergence (without it the random
    // /ID differs every call and would mask the comparison).
    opts_a.static_id = true;
    let mut out_a = Vec::new();
    write_pdf_with_options(&mut pdf_a, &mut out_a, &opts_a).unwrap();

    // Side B — feature path: Generate mode, but xref-form gate condition false.
    let mut pdf_b = Pdf::open(Cursor::new(source.clone())).unwrap();
    pdf_b.set_object(mutated_ref, mutated_value.clone());
    let mut opts_b = WriteOptions::default();
    opts_b.object_streams = ObjectStreamMode::Generate;
    opts_b.full_rewrite = false;
    opts_b.static_id = true;
    let mut out_b = Vec::new();
    write_pdf_with_options(&mut pdf_b, &mut out_b, &opts_b).unwrap();

    assert_eq!(
        out_a, out_b,
        "Generate over an xref-TABLE source must be byte-identical to the \
         legacy (Preserve) plain incremental write — the gate's Stream \
         condition is false so the new path must not engage"
    );
    // Sanity: no ObjStm container was emitted on the feature side.
    assert!(
        count_substrings(&out_b, b"/Type /ObjStm") == 0,
        "fallback output must contain no /Type /ObjStm container"
    );
}

/// Test 2 — gate condition disabled: **`object_streams == Generate`**
/// (Preserve and Disable both gate out by mode), Stream source.
///
/// The source IS xref-stream form and the mutation IS ObjStm-eligible, so the
/// ONLY thing keeping the gate shut for Preserve/Disable is the mode. Three
/// outputs from identical source + identical mutation:
///   - `out_preserve`  : object_streams = Preserve  (legacy baseline)
///   - `out_disable`   : object_streams = Disable
///   - `out_generate`  : object_streams = Generate  (gate ENGAGES here)
///
/// Asserts:
///   - `out_preserve == out_disable` — both gate out by mode; identical to
///     the pre-Task-5 plain incremental write.
///   - `out_generate != out_preserve` — the discriminator. This proves the
///     gate DOES fire when all three conditions hold, which is what makes the
///     Preserve/Disable equality a meaningful regression assertion rather
///     than a vacuous truth. (writer.rs:384 is the only incremental reader of
///     `object_streams`, so Preserve vs Disable run literally identical code
///     here; the assert_ne! against Generate is what gives this test teeth.)
#[test]
fn incremental_generate_fallback_preserve_and_disable_are_byte_identical() {
    let source = build_minimal_pdf_with_xref_stream();
    {
        let mut r = Cursor::new(&source);
        let loaded = load_xref_and_trailer(&mut r).unwrap();
        assert_eq!(
            loaded.last_xref_form,
            XrefForm::Stream,
            "fixture must be xref-STREAM form so only the mode keeps the gate shut"
        );
    }

    let mutated_ref = ObjectRef::new(2, 0);
    let mutated_value = Object::Dictionary({
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(b"Pages".to_vec()));
        d.insert("Count", Object::Integer(0));
        d.insert("Kids", Object::Array(Vec::new()));
        d.insert("FlpdfMutated", Object::Boolean(true));
        d
    });

    let write_with = |mode: ObjectStreamMode| -> Vec<u8> {
        let mut pdf = Pdf::open(Cursor::new(source.clone())).unwrap();
        pdf.set_object(mutated_ref, mutated_value.clone());
        let mut opts = WriteOptions::default();
        opts.object_streams = mode;
        opts.full_rewrite = false;
        // Deterministic /ID — see test 1 rationale.
        opts.static_id = true;
        let mut out = Vec::new();
        write_pdf_with_options(&mut pdf, &mut out, &opts).unwrap();
        out
    };

    let out_preserve = write_with(ObjectStreamMode::Preserve);
    let out_disable = write_with(ObjectStreamMode::Disable);
    let out_generate = write_with(ObjectStreamMode::Generate);

    assert_eq!(
        out_preserve, out_disable,
        "Preserve and Disable both gate out by mode — each must be byte-identical \
         to the legacy plain incremental write"
    );
    // Discriminator: proves the gate genuinely engages when ALL conditions
    // hold, so the equality above is a real (non-vacuous) regression check.
    assert_ne!(
        out_generate, out_preserve,
        "Generate (all gate conditions true) MUST diverge from the fallback — \
         if this fails the gate never engages and test coverage is vacuous"
    );
    assert!(
        count_substrings(&out_generate, b"/Type /ObjStm") > 0,
        "Generate side must actually emit an /ObjStm container (gate engaged)"
    );
    assert!(
        count_substrings(&out_preserve, b"/Type /ObjStm") == 0,
        "Preserve fallback must contain no /ObjStm container"
    );
}

/// Test 3 — gate condition disabled: **non-empty packable** (Generate +
/// Stream source, but the ONLY mutated object is ObjStm-INELIGIBLE).
///
/// The fixture is an xref-stream source with an extra content-stream object
/// (obj 3, an `Object::Stream`). Only obj 3 is mutated. `is_eligible_for_objstm`
/// rejects `Object::Stream` (writer/object_streams.rs:51), so `partition_objstm_eligible`
/// returns an EMPTY packable → `packable.is_empty()` → fallback to
/// `touched_object_refs.clone()`, `objstm_inc = None`.
///
/// Two sides, identical source + identical (stream) mutation:
///   - side A (legacy baseline): `Preserve`
///   - side B (feature path):    `Generate` (mode + Stream true, packable empty)
///
/// Outputs MUST be byte-identical: with an empty packable the Generate path
/// collapses to exactly the plain incremental write.
///
/// Non-tautological: sides differ by `object_streams` (Generate vs Preserve).
/// The Stream + Generate conditions ARE both true on side B, so only the
/// empty-packable condition keeps the gate shut — the predicate is genuinely
/// exercised. Verified empirically: side B emits no `/Type /ObjStm`.
#[test]
fn incremental_generate_fallback_empty_packable_is_byte_identical() {
    // xref-stream fixture extended with obj 3 = a FlateDecode content stream.
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::<usize>::new();

    offsets.push(bytes.len());
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets.push(bytes.len());
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n");

    let stream_payload = b"BT /F1 12 Tf (hello) Tj ET";
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(stream_payload).unwrap();
    let compressed = enc.finish().unwrap();
    offsets.push(bytes.len());
    bytes.extend_from_slice(
        format!(
            "3 0 obj\n<< /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&compressed);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_offset = bytes.len();
    // Entries are contiguous for objects 0..=4: obj0 free, obj1 catalog,
    // obj2 pages, obj3 content stream, obj4 the xref stream itself.
    let mut xref_entries = Vec::new();
    append_xref_stream_entry(&mut xref_entries, 0, 0, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[0] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[1] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, offsets[2] as u32, 0);
    append_xref_stream_entry(&mut xref_entries, 1, xref_offset as u32, 0);
    bytes.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 3 1] /Index [0 5] /Length {} >>\nstream\n",
            xref_entries.len()
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&xref_entries);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    let source = bytes;
    {
        let mut r = Cursor::new(&source);
        let loaded = load_xref_and_trailer(&mut r).unwrap();
        assert_eq!(
            loaded.last_xref_form,
            XrefForm::Stream,
            "fixture must be xref-STREAM form so only the empty-packable condition gates out"
        );
    }

    // The mutation: a NEW stream value for obj 3 — an Object::Stream, which
    // is ObjStm-INELIGIBLE, so the packable set is empty.
    let mutated_ref = ObjectRef::new(3, 0);
    let mutated_value = Object::Stream(flpdf::Stream {
        dict: {
            let mut d = Dictionary::new();
            d.insert("Length", Object::Integer(stream_payload.len() as i64));
            d.insert("FlpdfMutated", Object::Boolean(true));
            d
        },
        data: stream_payload.to_vec(),
    });

    // Side A — legacy baseline: Preserve, plain incremental.
    let mut pdf_a = Pdf::open(Cursor::new(source.clone())).unwrap();
    pdf_a.set_object(mutated_ref, mutated_value.clone());
    let mut opts_a = WriteOptions::default();
    opts_a.object_streams = ObjectStreamMode::Preserve;
    opts_a.full_rewrite = false;
    // Deterministic /ID — see test 1 rationale.
    opts_a.static_id = true;
    let mut out_a = Vec::new();
    write_pdf_with_options(&mut pdf_a, &mut out_a, &opts_a).unwrap();

    // Side B — feature path: Generate + Stream source, but empty packable.
    let mut pdf_b = Pdf::open(Cursor::new(source.clone())).unwrap();
    pdf_b.set_object(mutated_ref, mutated_value.clone());
    let mut opts_b = WriteOptions::default();
    opts_b.object_streams = ObjectStreamMode::Generate;
    opts_b.full_rewrite = false;
    opts_b.static_id = true;
    let mut out_b = Vec::new();
    write_pdf_with_options(&mut pdf_b, &mut out_b, &opts_b).unwrap();

    // Empirical proof the chosen mutation really is ineligible: no container.
    assert!(
        count_substrings(&out_b, b"/Type /ObjStm") == 0,
        "the only mutated object is a Stream (ObjStm-ineligible); the Generate \
         side must emit NO /Type /ObjStm container — if it does, the packable \
         was not empty and this test is comparing two engaging paths"
    );
    assert_eq!(
        out_a, out_b,
        "Generate with an empty packable (only ineligible object touched) must \
         be byte-identical to the legacy (Preserve) plain incremental write"
    );
}
