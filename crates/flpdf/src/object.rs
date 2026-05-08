use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    pub number: u32,
    pub generation: u16,
}

impl ObjectRef {
    pub fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjectRef),
}

impl Object {
    pub fn reference(object_ref: ObjectRef) -> Self {
        Self::Reference(object_ref)
    }

    pub(crate) fn write_pdf(&self, out: &mut Vec<u8>) {
        match self {
            Object::Null => out.extend_from_slice(b"null"),
            Object::Boolean(value) => {
                out.extend_from_slice(if *value { b"true" } else { b"false" })
            }
            Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
            Object::Name(name) => {
                out.push(b'/');
                out.extend_from_slice(name);
            }
            Object::String(value) => {
                out.push(b'(');
                out.extend_from_slice(value);
                out.push(b')');
            }
            Object::Array(values) => {
                out.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(b' ');
                    }
                    value.write_pdf(out);
                }
                out.push(b']');
            }
            Object::Dictionary(dict) => dict.write_pdf(out),
            Object::Stream(stream) => {
                stream.dict.write_pdf(out);
                out.extend_from_slice(b"\nstream\n");
                out.extend_from_slice(&stream.data);
                out.extend_from_slice(b"\nendstream");
            }
            Object::Reference(object_ref) => {
                out.extend_from_slice(object_ref.to_string().as_bytes())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dictionary {
    entries: BTreeMap<Vec<u8>, Object>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl AsRef<[u8]>, value: Object) {
        self.entries.insert(key.as_ref().to_vec(), value);
    }

    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&Object> {
        self.entries.get(key.as_ref())
    }

    pub fn get_ref(&self, key: impl AsRef<[u8]>) -> Option<ObjectRef> {
        match self.get(key) {
            Some(Object::Reference(object_ref)) => Some(*object_ref),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &Object)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_slice(), value))
    }

    pub(crate) fn write_pdf(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"<<");
        for (key, value) in self.iter() {
            out.extend_from_slice(b" /");
            out.extend_from_slice(key);
            out.push(b' ');
            value.write_pdf(out);
        }
        out.extend_from_slice(b" >>");
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dictionary,
    pub data: Vec<u8>,
}

impl Stream {
    pub fn new(dict: Dictionary, data: Vec<u8>) -> Self {
        Self { dict, data }
    }
}
