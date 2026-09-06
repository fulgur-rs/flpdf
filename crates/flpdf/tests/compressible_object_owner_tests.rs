//! qpdf getCompressibleObjGens removes stale generations from the live graph.

use flpdf::{ObjectRef, ObjectStreamMode, Pdf, PdfWriter};
use std::io::Cursor;

#[test]
fn generate_turns_a_retained_stale_generation_handle_into_direct_null() {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/compressible-stale-generation-alias.pdf")
            .to_vec(),
    ))
    .unwrap();
    let versions = pdf
        .root_handle()
        .unwrap()
        .try_get_key(b"/Versions")
        .unwrap();
    let old = versions.try_get_array_item(0).unwrap();
    let current = versions.try_get_array_item(1).unwrap();
    assert_eq!(old.object_ref(), Some(ObjectRef::new(3, 0)));
    assert_eq!(current.object_ref(), Some(ObjectRef::new(3, 1)));

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_object_stream_mode(ObjectStreamMode::Generate);
    writer.set_static_id(true);
    writer.set_output_memory().unwrap();
    writer.write().unwrap();

    // QPDF.cc:2423-2430 calls removeObject. QPDF.cc:1996-2005 changes
    // the existing allocation into a floating null, including held aliases.
    assert!(old.is_direct());
    assert!(old.is_null());
    assert_eq!(old.object_ref(), None);
    assert_eq!(current.object_ref(), Some(ObjectRef::new(3, 1)));
}
