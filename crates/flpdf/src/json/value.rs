use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::rc::Rc;

#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    #[error("{0}")]
    Type(String),
    #[error("{0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Default)]
pub struct Json(Option<Rc<RefCell<Members>>>);

impl std::fmt::Debug for Json {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Json")
            .field(&self.0.as_ref().map(|_| "<initialized>"))
            .finish()
    }
}

pub(crate) struct Members {
    pub(crate) value: Value,
    pub(crate) start: i64,
    pub(crate) end: i64,
}

#[allow(dead_code)] // Values are constructed by the later parser/tree layers.
pub(crate) enum Value {
    Dictionary {
        members: BTreeMap<Vec<u8>, Json>,
        parsed_keys: BTreeSet<Vec<u8>>,
    },
    Array(Vec<Json>),
    String {
        original: Vec<u8>,
        encoded: Vec<u8>,
    },
    Number(Vec<u8>),
    Bool(bool),
    Null,
    Blob(Rc<RefCell<Box<dyn FnMut(&mut dyn io::Write) -> io::Result<()>>>>),
}

pub(crate) enum ValueSnapshot {
    Dictionary(Vec<(Vec<u8>, Json)>),
    Array(Vec<Json>),
    String(Vec<u8>),
    Number(Vec<u8>),
    Bool(bool),
    Null,
    Blob(Rc<RefCell<Box<dyn FnMut(&mut dyn io::Write) -> io::Result<()>>>>),
}

pub(crate) enum ContainerOrBlobSnapshot {
    Dictionary(Vec<(Vec<u8>, Json)>),
    Array(Vec<Json>),
    Blob(Rc<RefCell<Box<dyn FnMut(&mut dyn io::Write) -> io::Result<()>>>>),
}

impl ValueSnapshot {
    pub(crate) fn into_container_or_blob(self) -> Option<ContainerOrBlobSnapshot> {
        match self {
            Self::Dictionary(members) => Some(ContainerOrBlobSnapshot::Dictionary(members)),
            Self::Array(values) => Some(ContainerOrBlobSnapshot::Array(values)),
            Self::Blob(writer) => Some(ContainerOrBlobSnapshot::Blob(writer)),
            Self::String(_) | Self::Number(_) | Self::Bool(_) | Self::Null => None,
        }
    }
}

impl Json {
    pub const LATEST: i32 = 2;

    pub fn make_string(value: impl AsRef<[u8]>) -> Self {
        let original = value.as_ref().to_vec();
        Self::with_value(Value::String {
            encoded: encode_string(&original),
            original,
        })
    }

    pub fn make_int(value: i64) -> Self {
        Self::make_number(value.to_string())
    }

    pub fn make_real(value: f64) -> Self {
        let mut encoded = format!("{value:.6}");
        while encoded.ends_with('0') && encoded.len() > 1 {
            encoded.pop();
        }
        if encoded.ends_with('.') && encoded.len() > 1 {
            encoded.pop();
        }
        Self::make_number(encoded)
    }

    pub fn make_number(encoded: impl AsRef<[u8]>) -> Self {
        Self::with_value(Value::Number(encoded.as_ref().to_vec()))
    }

    pub fn make_bool(value: bool) -> Self {
        Self::with_value(Value::Bool(value))
    }

    pub fn make_null() -> Self {
        Self::with_value(Value::Null)
    }

    pub fn get_string(&self) -> Option<Vec<u8>> {
        let members = self.0.as_ref()?.borrow();
        let Value::String { original, .. } = &members.value else {
            return None;
        };
        Some(original.clone())
    }

    pub fn get_number(&self) -> Option<Vec<u8>> {
        let members = self.0.as_ref()?.borrow();
        let Value::Number(value) = &members.value else {
            return None;
        };
        Some(value.clone())
    }

    pub fn get_bool(&self) -> Option<bool> {
        let members = self.0.as_ref()?.borrow();
        let Value::Bool(value) = members.value else {
            return None;
        };
        Some(value)
    }

    pub fn is_null(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|members| matches!(members.borrow().value, Value::Null))
    }

    pub fn set_start(&self, start: i64) {
        if let Some(members) = &self.0 {
            members.borrow_mut().start = start;
        }
    }

    pub fn set_end(&self, end: i64) {
        if let Some(members) = &self.0 {
            members.borrow_mut().end = end;
        }
    }

    pub fn start(&self) -> i64 {
        self.0.as_ref().map_or(0, |members| members.borrow().start)
    }

    pub fn end(&self) -> i64 {
        self.0.as_ref().map_or(0, |members| members.borrow().end)
    }

    fn with_value(value: Value) -> Self {
        Self(Some(Rc::new(RefCell::new(Members {
            value,
            start: 0,
            end: 0,
        }))))
    }

    pub(crate) fn value_snapshot(&self) -> Option<ValueSnapshot> {
        let members = self.0.as_ref()?.borrow();
        Some(match &members.value {
            Value::Dictionary {
                members,
                parsed_keys,
            } => {
                let _ = parsed_keys;
                ValueSnapshot::Dictionary(
                    members
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )
            }
            Value::Array(values) => ValueSnapshot::Array(values.clone()),
            Value::String { encoded, .. } => ValueSnapshot::String(encoded.clone()),
            Value::Number(value) => ValueSnapshot::Number(value.clone()),
            Value::Bool(value) => ValueSnapshot::Bool(*value),
            Value::Null => ValueSnapshot::Null,
            Value::Blob(writer) => ValueSnapshot::Blob(writer.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;

    use super::{Json, Value};

    #[test]
    fn scalar_constructors_preserve_qpdf_encoded_and_original_values() {
        let string = Json::make_string(b"quote \" slash \\ control \x01");
        assert_eq!(
            string.get_string(),
            Some(b"quote \" slash \\ control \x01".to_vec())
        );
        assert_eq!(
            string.unparse().unwrap(),
            b"\"quote \\\" slash \\\\ control \\u0001\""
        );
        assert_eq!(
            Json::make_string(b"\x08\x0c\n\r\t\x1f").unparse().unwrap(),
            b"\"\\b\\f\\n\\r\\t\\u001f\""
        );

        assert_eq!(Json::make_int(-42).unparse().unwrap(), b"-42");
        assert_eq!(Json::make_real(2.5).unparse().unwrap(), b"2.5");
        assert_eq!(Json::make_real(0.0).unparse().unwrap(), b"0");
        assert_eq!(Json::make_null().unparse().unwrap(), b"null");
        assert_eq!(Json::make_null().get_bool(), None);
        assert_eq!(Json::LATEST, 2);
    }

    #[test]
    fn offsets_are_shared_by_cloned_initialized_handles_and_ignored_by_default() {
        let value = Json::make_bool(false);
        let clone = value.clone();
        value.set_start(4);
        clone.set_end(9);
        assert_eq!((value.start(), value.end()), (4, 9));

        let default = Json::default();
        default.set_start(1);
        default.set_end(2);
        assert_eq!((default.start(), default.end()), (0, 0));
        assert_eq!(format!("{default:?}"), "Json(None)");
    }

    #[test]
    fn tree_snapshots_write_qpdf_indentation_and_sorted_encoded_keys() {
        let tree = Json::with_value(Value::Dictionary {
            members: BTreeMap::from([
                (b"a".to_vec(), Json::make_number(b"2")),
                (
                    b"z".to_vec(),
                    Json::with_value(Value::Array(vec![Json::make_bool(true)])),
                ),
            ]),
            parsed_keys: BTreeSet::from([b"a".to_vec()]),
        });

        assert_eq!(
            tree.unparse().unwrap(),
            b"{\n  \"a\": 2,\n  \"z\": [\n    true\n  ]\n}"
        );
    }

    #[test]
    fn empty_and_multi_element_containers_write_qpdf_delimiters() {
        let empty_dictionary = Json::with_value(Value::Dictionary {
            members: BTreeMap::new(),
            parsed_keys: BTreeSet::new(),
        });
        assert_eq!(empty_dictionary.unparse().unwrap(), b"{}");

        let array = Json::with_value(Value::Array(vec![Json::make_int(1), Json::make_int(2)]));
        assert_eq!(array.unparse().unwrap(), b"[\n  1,\n  2\n]");

        let empty_array = Json::with_value(Value::Array(Vec::new()));
        assert_eq!(empty_array.unparse().unwrap(), b"[]");
    }

    #[test]
    fn blob_writer_base64_encodes_the_bytes_produced_by_its_callback() {
        let blob = Json::with_value(Value::Blob(Rc::new(std::cell::RefCell::new(Box::new(
            |out| out.write_all(b"\x00\xff"),
        )))));

        assert_eq!(blob.unparse().unwrap(), b"\"AP8=\"");
    }
}

fn encode_string(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(value.len());
    for &byte in value {
        match byte {
            b'\\' => encoded.extend_from_slice(&[b'\\', b'\\']),
            b'"' => encoded.extend_from_slice(&[b'\\', b'"']),
            b'\x08' => encoded.extend_from_slice(&[b'\\', b'b']),
            b'\x0c' => encoded.extend_from_slice(&[b'\\', b'f']),
            b'\n' => encoded.extend_from_slice(&[b'\\', b'n']),
            b'\r' => encoded.extend_from_slice(&[b'\\', b'r']),
            b'\t' => encoded.extend_from_slice(&[b'\\', b't']),
            0x00..=0x1f => {
                encoded.extend_from_slice(if byte < 0x10 {
                    &[b'\\', b'u', b'0', b'0', b'0']
                } else {
                    &[b'\\', b'u', b'0', b'0', b'1']
                });
                encoded.push(b"0123456789abcdef"[(byte & 0x0f) as usize]);
            }
            _ => encoded.push(byte),
        }
    }
    encoded
}
