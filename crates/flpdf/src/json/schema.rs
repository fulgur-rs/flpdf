//! qpdf correspondence: JSON.cc schema validation responsibilities.

use std::collections::BTreeMap;

use super::{Json, JsonMessage};

/// Flags that alter qpdf-compatible JSON schema validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaFlags(u64);

impl SchemaFlags {
    pub const NONE: Self = Self(0);
    pub const OPTIONAL: Self = Self(1);

    pub(crate) fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for SchemaFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl Json {
    /// Check this value against a qpdf JSON schema with no flags.
    pub fn check_schema(&self, schema: &Json, errors: &mut Vec<JsonMessage>) -> bool {
        self.check_schema_with_flags(schema, SchemaFlags::NONE, errors)
    }

    /// Check this value against a qpdf JSON schema.
    pub fn check_schema_with_flags(
        &self,
        schema: &Json,
        flags: SchemaFlags,
        errors: &mut Vec<JsonMessage>,
    ) -> bool {
        if self.value_snapshot().is_none() {
            return false;
        }
        check_schema_internal(self, schema, flags, errors, b"")
    }
}

fn check_schema_internal(
    value: &Json,
    schema: &Json,
    flags: SchemaFlags,
    errors: &mut Vec<JsonMessage>,
    prefix: &[u8],
) -> bool {
    let value_array = array_items(value);
    let value_dictionary = dictionary_items(value);
    let schema_array = array_items(schema);
    let schema_dictionary = dictionary_items(schema);
    if let Some(schema_dictionary) = schema_dictionary {
        let Some(value_dictionary) = value_dictionary else {
            errors.push(described_error(prefix, b" is supposed to be a dictionary"));
            return false;
        };

        let pattern_schema = if schema_dictionary.len() == 1 {
            schema_dictionary
                .first_key_value()
                .and_then(|(key, item_schema)| is_pattern_key(key).then_some(item_schema))
        } else {
            None
        };
        if let Some(pattern_schema) = pattern_schema {
            for (key, item) in value_dictionary {
                check_schema_internal(
                    &item,
                    pattern_schema,
                    flags,
                    errors,
                    &append_path(prefix, &key),
                );
            }
        } else {
            for (key, item_schema) in &schema_dictionary {
                if let Some(item) = value_dictionary.get(key) {
                    check_schema_internal(
                        item,
                        item_schema,
                        flags,
                        errors,
                        &append_path(prefix, key),
                    );
                } else if !flags.contains(SchemaFlags::OPTIONAL) {
                    errors.push(key_error(
                        prefix,
                        key,
                        b"is present in schema but missing in object",
                    ));
                }
            }
            for (key, _) in value_dictionary {
                if !schema_dictionary.contains_key(&key) {
                    errors.push(key_error(
                        prefix,
                        &key,
                        b"is not present in schema but appears in object",
                    ));
                }
            }
        }
    } else if let Some(schema_array) = schema_array {
        if schema_array.len() == 1 {
            if let Some(value_array) = value_array {
                for (index, item) in value_array.into_iter().enumerate() {
                    let mut item_prefix = prefix.to_vec();
                    item_prefix.push(b'.');
                    item_prefix.extend_from_slice(index.to_string().as_bytes());
                    check_schema_internal(&item, &schema_array[0], flags, errors, &item_prefix);
                }
            } else {
                check_schema_internal(value, &schema_array[0], flags, errors, prefix);
            }
        } else if value_array
            .as_ref()
            .is_none_or(|items| items.len() != schema_array.len())
        {
            let mut suffix = b" is supposed to be an array of length ".to_vec();
            suffix.extend_from_slice(schema_array.len().to_string().as_bytes());
            errors.push(described_error(prefix, &suffix));
            return false;
        } else {
            let value_array =
                value_array.expect("length check guarantees the checked value is an array");
            for (index, item) in value_array.into_iter().enumerate() {
                let mut item_prefix = prefix.to_vec();
                item_prefix.push(b'.');
                item_prefix.extend_from_slice(index.to_string().as_bytes());
                check_schema_internal(&item, &schema_array[index], flags, errors, &item_prefix);
            }
        }
    } else if schema.get_string().is_none() {
        errors.push(described_error(
            prefix,
            b" schema value is not dictionary, array, or string",
        ));
        return false;
    }

    errors.is_empty()
}

fn dictionary_items(value: &Json) -> Option<BTreeMap<Vec<u8>, Json>> {
    let mut items = BTreeMap::new();
    value
        .for_each_dict_item(|key, item| {
            items.insert(key.to_vec(), item);
        })
        .then_some(items)
}

fn array_items(value: &Json) -> Option<Vec<Json>> {
    let mut items = Vec::new();
    value
        .for_each_array_item(|item| items.push(item))
        .then_some(items)
}

fn is_pattern_key(key: &[u8]) -> bool {
    key.len() > 2 && key[0] == b'<' && key[key.len() - 1] == b'>'
}

fn append_path(prefix: &[u8], key: &[u8]) -> Vec<u8> {
    let mut path = prefix.to_vec();
    path.push(b'.');
    path.extend_from_slice(key);
    path
}

fn described_prefix(prefix: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        b"top-level object".to_vec()
    } else {
        let mut description = b"json key \"".to_vec();
        description.extend_from_slice(prefix);
        description.push(b'"');
        description
    }
}

fn key_error(prefix: &[u8], key: &[u8], suffix: &[u8]) -> JsonMessage {
    let mut message = described_prefix(prefix);
    message.extend_from_slice(b": key \"");
    message.extend_from_slice(key);
    message.extend_from_slice(b"\" ");
    message.extend_from_slice(suffix);
    JsonMessage::from_bytes(message)
}

fn described_error(prefix: &[u8], suffix: &[u8]) -> JsonMessage {
    let mut message = described_prefix(prefix);
    message.extend_from_slice(suffix);
    JsonMessage::from_bytes(message)
}
