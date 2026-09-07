use std::path::Path;

#[test]
fn plain_disable_uses_the_live_queue_consumer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let plain = std::fs::read_to_string(root.join("writer/plain/mod.rs")).unwrap();
    let body = std::fs::read_to_string(root.join("writer/plain/body.rs")).unwrap();

    assert!(plain.contains("write_plain_live_disable"));
    assert!(body.contains("struct LiveQueue"));
    assert!(body.contains("emit_live_disable"));
    assert!(body.contains("enqueue_handle"));
}
