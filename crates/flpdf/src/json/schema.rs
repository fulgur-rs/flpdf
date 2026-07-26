use super::Json;

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
    pub fn check_schema(&self, schema: &Json, errors: &mut Vec<String>) -> bool {
        self.check_schema_with_flags(schema, SchemaFlags::NONE, errors)
    }

    /// Check this value against a qpdf JSON schema.
    pub fn check_schema_with_flags(
        &self,
        schema: &Json,
        flags: SchemaFlags,
        errors: &mut Vec<String>,
    ) -> bool {
        if self.value_snapshot().is_none() {
            return false;
        }
        check_schema_internal(self, schema, flags, errors, "")
    }
}

fn check_schema_internal(
    value: &Json,
    schema: &Json,
    flags: SchemaFlags,
    errors: &mut Vec<String>,
    prefix: &str,
) -> bool {
    let value_array = array_items(value);
    let value_dictionary = dictionary_items(value);
    let schema_array = array_items(schema);
    let schema_dictionary = dictionary_items(schema);
    let error_prefix = if prefix.is_empty() {
        "top-level object".to_owned()
    } else {
        format!("json key \"{prefix}\"")
    };

    if let Some(schema_dictionary) = schema_dictionary {
        let Some(value_dictionary) = value_dictionary else {
            errors.push(format!("{error_prefix} is supposed to be a dictionary"));
            return false;
        };

        if schema_dictionary.len() == 1 && is_pattern_key(&schema_dictionary[0].0) {
            let pattern_schema = &schema_dictionary[0].1;
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
                if let Some((_, item)) = value_dictionary
                    .iter()
                    .find(|(value_key, _)| value_key == key)
                {
                    check_schema_internal(
                        item,
                        item_schema,
                        flags,
                        errors,
                        &append_path(prefix, key),
                    );
                } else if !flags.contains(SchemaFlags::OPTIONAL) {
                    errors.push(format!(
                        "{error_prefix}: key \"{}\" is present in schema but missing in object",
                        key_name(key)
                    ));
                }
            }
            for (key, _) in value_dictionary {
                if !schema_dictionary
                    .iter()
                    .any(|(schema_key, _)| schema_key == &key)
                {
                    errors.push(format!(
                        "{error_prefix}: key \"{}\" is not present in schema but appears in object",
                        key_name(&key)
                    ));
                }
            }
        }
    } else if let Some(schema_array) = schema_array {
        if schema_array.len() == 1 {
            if let Some(value_array) = value_array {
                for (index, item) in value_array.into_iter().enumerate() {
                    check_schema_internal(
                        &item,
                        &schema_array[0],
                        flags,
                        errors,
                        &format!("{prefix}.{index}"),
                    );
                }
            } else {
                check_schema_internal(value, &schema_array[0], flags, errors, prefix);
            }
        } else if value_array
            .as_ref()
            .is_none_or(|items| items.len() != schema_array.len())
        {
            errors.push(format!(
                "{error_prefix} is supposed to be an array of length {}",
                schema_array.len()
            ));
            return false;
        } else {
            let value_array =
                value_array.expect("length check guarantees the checked value is an array");
            for (index, item) in value_array.into_iter().enumerate() {
                check_schema_internal(
                    &item,
                    &schema_array[index],
                    flags,
                    errors,
                    &format!("{prefix}.{index}"),
                );
            }
        }
    } else if schema.get_string().is_none() {
        errors.push(format!(
            "{error_prefix} schema value is not dictionary, array, or string"
        ));
        return false;
    }

    errors.is_empty()
}

fn dictionary_items(value: &Json) -> Option<Vec<(Vec<u8>, Json)>> {
    let mut items = Vec::new();
    value
        .for_each_dict_item(|key, item| items.push((key.to_vec(), item)))
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

fn append_path(prefix: &str, key: &[u8]) -> String {
    format!("{prefix}.{}", key_name(key))
}

fn key_name(key: &[u8]) -> String {
    String::from_utf8_lossy(key).into_owned()
}
