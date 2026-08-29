use flpdf::{ObjectRef, Pdf};

fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let max = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
    for &(number, body) in objects {
        offsets.insert(number, out.len() as u64);
        out.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..=max {
        match offsets.get(&number) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            max + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn public_copy_foreign_object_preserves_shared_child_identity() {
    let source_bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /A /Child 6 0 R >>"),
            (5, "<< /Type /B /Child 6 0 R >>"),
            (6, "<< /Type /Shared >>"),
        ],
        1,
    );
    let mut source = Pdf::open_mem_owned(source_bytes).expect("source PDF");
    let mut target = Pdf::empty().expect("target PDF");

    let first = target
        .copy_foreign_object(&source.get_object_handle(ObjectRef::new(4, 0)))
        .expect("copy first foreign object");
    let second = target
        .copy_foreign_object(&source.get_object_handle(ObjectRef::new(5, 0)))
        .expect("copy second foreign object");

    let first_child = first.get_key(b"/Child");
    let second_child = second.get_key(b"/Child");
    assert!(first_child.is_same_object_as(&second_child));
    assert_eq!(first_child.object_ref(), second_child.object_ref());
}

#[test]
fn public_copy_foreign_object_accepts_a_canonical_direct_value_child() {
    let mut source = Pdf::empty().expect("source PDF");
    let mut target = Pdf::empty().expect("target PDF");
    let source_root = source.root_handle().expect("source root");
    let direct = source_root.get_key(b"/Pages");
    assert!(direct.is_indirect());

    let copied = target
        .copy_foreign_object(
            &source.get_object_handle(direct.object_ref().expect("source Pages reference")),
        )
        .expect("copy canonical foreign child");
    assert!(copied.is_null(), "the Pages boundary is replaced by null");
}
