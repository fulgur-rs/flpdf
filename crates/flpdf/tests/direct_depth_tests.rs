use flpdf::{ObjectHandle, ObjectJsonError};

const DIRECT_DEPTH: usize = 502;

fn deeply_nested_array(depth: usize) -> ObjectHandle {
    (0..depth).fold(ObjectHandle::integer(1), |child, _| {
        ObjectHandle::array(vec![child])
    })
}

fn qpdf_nested_array_unparse(depth: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(depth * 4 + 1);
    for _ in 0..depth {
        bytes.extend_from_slice(b"[ ");
    }
    bytes.push(b'1');
    for _ in 0..depth {
        bytes.extend_from_slice(b" ]");
    }
    bytes
}

fn qpdf_nested_array_json(depth: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 * depth * depth + 4 * depth + 1);
    bytes.push(b'[');
    for level in 1..=depth {
        bytes.push(b'\n');
        bytes.resize(bytes.len() + 2 * level, b' ');
        bytes.push(if level == depth { b'1' } else { b'[' });
    }
    for level in (0..depth).rev() {
        bytes.push(b'\n');
        bytes.resize(bytes.len() + 2 * level, b' ');
        bytes.push(b']');
    }
    bytes
}

#[test]
fn shallow_copy_and_unparse_accept_programmatic_graphs_beyond_parser_depth() {
    let original = deeply_nested_array(DIRECT_DEPTH);
    let copied = original.shallow_copy().unwrap();
    let expected = qpdf_nested_array_unparse(DIRECT_DEPTH);

    assert_eq!(original.unparse(), expected);
    assert_eq!(original.unparse_resolved(), expected);
    assert_eq!(original.try_unparse_resolved().unwrap(), expected);
    assert_eq!(copied.unparse(), expected);
    assert_eq!(copied.unparse_resolved(), expected);
    assert_eq!(copied.try_unparse_resolved().unwrap(), expected);
}

#[test]
fn make_direct_uses_qpdf_object_generations_and_accepts_deep_direct_graphs() {
    let mut value = deeply_nested_array(DIRECT_DEPTH);
    value.make_direct(false).unwrap();
    assert_eq!(
        value.try_unparse_resolved().unwrap(),
        qpdf_nested_array_unparse(DIRECT_DEPTH)
    );
}

#[test]
fn make_direct_preserves_qpdf_uninitialized_receiver_error() {
    let mut value = ObjectHandle::uninitialized();
    let error = value
        .make_direct(false)
        .expect_err("qpdf asserts initialization before reading ObjGen");

    assert!(matches!(
        error,
        flpdf::Error::Internal(message)
            if message == "operation attempted on uninitialized QPDFObjectHandle"
    ));
}

#[test]
fn direct_json_writer_has_no_container_depth_cap() {
    let value = deeply_nested_array(DIRECT_DEPTH);
    let expected = qpdf_nested_array_json(DIRECT_DEPTH);
    let mut bytes = Vec::new();
    let mut output = flpdf::pipeline::PlString::new("direct JSON depth", None, &mut bytes);

    value.write_json(2, &mut output, true, 0).unwrap();

    assert_eq!(bytes, expected);
}

#[test]
fn get_json_keeps_the_qpdf_input_parser_depth_limit() {
    let value = deeply_nested_array(DIRECT_DEPTH);
    let error = value
        .get_json(2, true)
        .expect_err("getJSON reparses the serialized value with qpdf's JSON parser");

    assert!(matches!(
        error,
        ObjectJsonError::Json(message)
            if message == "JSON: offset 251501: maximum object depth exceeded"
    ));
}
