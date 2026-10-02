#[test]
fn parsedoffset_walk_uses_resolving_container_accessors() {
    let source = include_str!("../src/metadata.rs").replace("\r\n", "\n");
    let start = source.find("fn walk(").expect("metadata walk start");
    let end = source[start..]
        .find("fn stream_group(")
        .map(|offset| start + offset)
        .expect("metadata walk end");
    let walk = &source[start..end];

    assert!(walk.contains("object.try_is_array()?"));
    assert!(walk.contains("object.try_get_array_as_vector()?"));
    assert!(walk.contains("object.try_is_dictionary()?"));
    assert!(walk.contains("object.try_get_keys()?"));
    assert!(walk.contains("object.try_get_key(&key)?"));
    assert!(walk.contains("item.is_indirect()"));
    assert!(!walk.contains(".as_array()"));
    assert!(!walk.contains(".as_dictionary()"));
    assert!(!walk.contains(".is_null()"));
}
