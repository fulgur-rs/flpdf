//! Contract tests for the writer renumber test-only handle cutover.

#[test]
fn rewrite_renumber_tests_do_not_use_legacy_object_snapshots() {
    let source = include_str!("../src/writer/rewrite_renumber.rs");
    let tests = source
        .split_once("#[cfg(test)]")
        .expect("rewrite_renumber must have a test module")
        .1;

    assert!(
        !tests.contains("resolve_object("),
        "rewrite_renumber tests still use the legacy Object snapshot route"
    );
    assert!(
        tests.contains("get_object_handle"),
        "rewrite_renumber tests must use canonical handles"
    );
    assert!(
        tests.contains("pdf.resolve("),
        "rewrite_renumber tests must explicitly resolve canonical handles"
    );
}
