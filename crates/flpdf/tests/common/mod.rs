//! Shared test helpers for the helper-API integration tests.
//!
//! Lives in a `common/` subdirectory so Cargo treats it as a module included
//! by each test binary rather than as its own test target.

/// Build a PDF from a set of already-serialised indirect objects.
///
/// `objects` is a slice of `(object_number, "<<...>>" body)` where the body is
/// everything between `N 0 obj\n` and `\nendobj\n`. The cross-reference table
/// and trailer are generated automatically; `root` names the `/Root` object.
///
/// Object offsets are kept in a `BTreeMap` so xref generation is O(N log N).
pub fn build_pdf(objects: &[(u32, String)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let max_num = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut offsets: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    for (num, body) in objects {
        offsets.insert(*num, out.len() as u64);
        out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }
    let total = max_num as usize + 1;
    let xref_start = out.len() as u64;
    let mut xref = format!("xref\n0 {total}\n0000000000 65535 f \n");
    for i in 1..=max_num {
        if let Some(off) = offsets.get(&i) {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        } else {
            xref.push_str("0000000000 65535 f \n");
        }
    }
    out.extend_from_slice(xref.as_bytes());
    let trailer =
        format!("trailer\n<< /Size {total} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}
