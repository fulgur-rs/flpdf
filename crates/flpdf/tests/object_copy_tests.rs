//! Integration tests for [`flpdf::object_copy::copy_objects`].

use flpdf::{copy_objects, Object, ObjectRef, Pdf};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// Minimal PDF builder helpers
// ---------------------------------------------------------------------------

/// Build a PDF from `(number, body)` object definitions plus a `/Root` number.
///
/// `body` is the literal text between `N 0 obj` and `endobj` (e.g.
/// `"<< /Type /Catalog /Pages 2 0 R >>"`). Object numbers need not be
/// contiguous; gaps are emitted as free xref entries.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);

    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n"); // object 0
    for n in 1..=max {
        match offsets.get(&n) {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Minimal valid target document: catalog(1) / pages(2) / page(3). Max number 3.
fn build_target_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

/// Recursively test whether `obj` contains `needle` anywhere in its subtree.
fn object_contains(obj: &Object, needle: &Object) -> bool {
    if obj == needle {
        return true;
    }
    match obj {
        Object::Array(items) => items.iter().any(|i| object_contains(i, needle)),
        Object::Dictionary(d) => d.iter().any(|(_, v)| object_contains(v, needle)),
        Object::Stream(s) => s.dict.iter().any(|(_, v)| object_contains(v, needle)),
        _ => false,
    }
}

fn refset(refs: &[ObjectRef]) -> BTreeSet<ObjectRef> {
    refs.iter().copied().collect()
}

// ---------------------------------------------------------------------------
// Task 1: chain copy with fresh numbers
// ---------------------------------------------------------------------------

#[test]
fn copies_chain_with_fresh_numbers() {
    // Source: 4 -> 5 -> 6 (a -> b -> c), atop a minimal valid page.
    let src = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /A /Next 5 0 R >>"),
            (5, "<< /Type /B /Next 6 0 R >>"),
            (6, "<< /Type /C >>"),
        ],
        1,
    );
    let tgt = build_target_pdf();
    let mut source = Pdf::open_mem(&src).unwrap();
    let mut target = Pdf::open_mem(&tgt).unwrap();

    let refs = refset(&[
        ObjectRef::new(4, 0),
        ObjectRef::new(5, 0),
        ObjectRef::new(6, 0),
    ]);

    let map = copy_objects(&mut source, &mut target, &refs).unwrap();

    assert_eq!(map.len(), 3, "one map entry per input ref");

    // All target numbers are fresh (greater than target's original max = 3).
    for t in map.values() {
        assert!(t.number > 3, "fresh target number, got {}", t.number);
    }

    // A's copy references map[5]; B's copy references map[6].
    let a = target.resolve(map[&ObjectRef::new(4, 0)]).unwrap();
    assert!(
        object_contains(&a, &Object::Reference(map[&ObjectRef::new(5, 0)])),
        "A's copy must reference the remapped B"
    );
    let b = target.resolve(map[&ObjectRef::new(5, 0)]).unwrap();
    assert!(
        object_contains(&b, &Object::Reference(map[&ObjectRef::new(6, 0)])),
        "B's copy must reference the remapped C"
    );
}
