use flpdf::{Dictionary, Object, ObjectHandle};

/// Lift a test fixture dictionary into the ObjectHandle-native filter input.
pub fn dictionary(dict: &Dictionary) -> ObjectHandle {
    ObjectHandle::dictionary(
        dict.iter()
            .map(|(key, value)| (canonical_key(key), value_handle(value)))
            .collect(),
    )
}

fn canonical_key(key: &[u8]) -> Vec<u8> {
    if key.starts_with(b"/") {
        key.to_vec()
    } else {
        let mut canonical = Vec::with_capacity(key.len() + 1);
        canonical.push(b'/');
        canonical.extend_from_slice(key);
        canonical
    }
}

fn value_handle(value: &Object) -> ObjectHandle {
    match value {
        Object::Null => ObjectHandle::null(),
        Object::Boolean(value) => ObjectHandle::boolean(*value),
        Object::Integer(value) => ObjectHandle::integer(*value),
        Object::Real(value) => ObjectHandle::real(*value),
        Object::RealLiteral { value, .. } => ObjectHandle::real(*value),
        Object::Name(value) => ObjectHandle::name(value.clone()),
        Object::String(value) => ObjectHandle::string(value.clone()),
        Object::Array(values) => ObjectHandle::array(values.iter().map(value_handle).collect()),
        Object::Dictionary(dictionary) => ObjectHandle::dictionary(
            dictionary
                .iter()
                .map(|(key, value)| (canonical_key(key), value_handle(value)))
                .collect(),
        ),
        Object::Reference(_) | Object::Stream(_) | Object::Operator(_) | Object::InlineImage(_) => {
            ObjectHandle::boolean(false)
        }
    }
}
