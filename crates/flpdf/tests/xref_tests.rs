use flpdf::{load_xref_and_trailer, ObjectRef};
use std::fs::File;
use std::io::BufReader;

#[test]
fn loads_xref_table_and_trailer() {
    let file = File::open("../../tests/fixtures/minimal.pdf").unwrap();
    let mut reader = BufReader::new(file);
    let loaded = load_xref_and_trailer(&mut reader).unwrap();

    assert_eq!(loaded.version, "1.7");
    assert_eq!(loaded.startxref, 110);
    assert_eq!(loaded.entries.get(&ObjectRef::new(1, 0)).copied(), Some(9));
    assert_eq!(loaded.entries.get(&ObjectRef::new(2, 0)).copied(), Some(58));
    assert_eq!(loaded.trailer.get_ref("Root"), Some(ObjectRef::new(1, 0)));
}
