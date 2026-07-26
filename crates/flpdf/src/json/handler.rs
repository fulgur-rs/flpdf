use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::Json;

pub type SharedJsonHandler = Rc<RefCell<JsonHandler>>;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("{0}")]
pub struct JsonHandlerError(pub String);

type JsonCallback = Box<dyn FnMut(&[u8], Json)>;
type BytesCallback = Box<dyn FnMut(&[u8], &[u8])>;
type PathCallback = Box<dyn FnMut(&[u8])>;
type BoolCallback = Box<dyn FnMut(&[u8], bool)>;

#[derive(Default)]
pub struct JsonHandler {
    any: Option<JsonCallback>,
    null: Option<PathCallback>,
    string: Option<BytesCallback>,
    number: Option<BytesCallback>,
    boolean: Option<BoolCallback>,
    dictionary_start: Option<JsonCallback>,
    dictionary_end: Option<PathCallback>,
    array_start: Option<JsonCallback>,
    array_end: Option<PathCallback>,
    dictionary_keys: BTreeMap<Vec<u8>, SharedJsonHandler>,
    fallback_dictionary: Option<SharedJsonHandler>,
    array_item: Option<SharedJsonHandler>,
    fallback: Option<SharedJsonHandler>,
}

impl JsonHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> SharedJsonHandler {
        Rc::new(RefCell::new(Self::new()))
    }

    pub fn add_any_handler(&mut self, callback: impl FnMut(&[u8], Json) + 'static) {
        self.any = Some(Box::new(callback));
    }

    pub fn add_null_handler(&mut self, callback: impl FnMut(&[u8]) + 'static) {
        self.null = Some(Box::new(callback));
    }

    pub fn add_string_handler(&mut self, callback: impl FnMut(&[u8], &[u8]) + 'static) {
        self.string = Some(Box::new(callback));
    }

    pub fn add_number_handler(&mut self, callback: impl FnMut(&[u8], &[u8]) + 'static) {
        self.number = Some(Box::new(callback));
    }

    pub fn add_bool_handler(&mut self, callback: impl FnMut(&[u8], bool) + 'static) {
        self.boolean = Some(Box::new(callback));
    }

    pub fn add_dictionary_handlers(
        &mut self,
        start: impl FnMut(&[u8], Json) + 'static,
        end: impl FnMut(&[u8]) + 'static,
    ) {
        self.dictionary_start = Some(Box::new(start));
        self.dictionary_end = Some(Box::new(end));
    }

    pub fn add_dictionary_key_handler(
        &mut self,
        key: impl AsRef<[u8]>,
        handler: SharedJsonHandler,
    ) {
        self.dictionary_keys.insert(key.as_ref().to_vec(), handler);
    }

    pub fn add_fallback_dictionary_handler(&mut self, handler: SharedJsonHandler) {
        self.fallback_dictionary = Some(handler);
    }

    pub fn add_array_handlers(
        &mut self,
        start: impl FnMut(&[u8], Json) + 'static,
        end: impl FnMut(&[u8]) + 'static,
        item: SharedJsonHandler,
    ) {
        self.array_start = Some(Box::new(start));
        self.array_end = Some(Box::new(end));
        self.array_item = Some(item);
    }

    pub fn add_fallback_handler(&mut self, handler: SharedJsonHandler) {
        self.fallback = Some(handler);
    }

    pub fn handle(&mut self, path: &[u8], value: Json) -> Result<(), JsonHandlerError> {
        if let Some(callback) = self.any.as_mut() {
            callback(path, value);
            return Ok(());
        }

        if value.is_null() {
            if let Some(callback) = self.null.as_mut() {
                callback(path);
                return Ok(());
            }
        }

        if let (Some(callback), Some(string)) = (self.string.as_mut(), value.get_string()) {
            callback(path, &string);
            return Ok(());
        }

        if let (Some(callback), Some(number)) = (self.number.as_mut(), value.get_number()) {
            callback(path, &number);
            return Ok(());
        }

        if let (Some(callback), Some(boolean)) = (self.boolean.as_mut(), value.get_bool()) {
            callback(path, boolean);
            return Ok(());
        }

        if value.is_dictionary() {
            if let Some(callback) = self.dictionary_start.as_mut() {
                callback(path, value.clone());
                let mut path_base = path.to_vec();
                if path_base != b"." {
                    path_base.push(b'.');
                }
                let mut items = Vec::new();
                value.for_each_dict_item(|key, item| items.push((key.to_vec(), item)));
                for (key, item) in items {
                    let mut item_path = path_base.clone();
                    item_path.extend_from_slice(&key);
                    if let Some(handler) = self.dictionary_keys.get(&key) {
                        handler.borrow_mut().handle(&item_path, item)?;
                    } else if let Some(handler) = &self.fallback_dictionary {
                        handler.borrow_mut().handle(&item_path, item)?;
                    } else {
                        return Err(unexpected_key(&key, path));
                    }
                }
                self.dictionary_end
                    .as_mut()
                    .expect("dictionary end handler is paired with start")(path);
                return Ok(());
            }
        }

        if value.is_array() {
            if let Some(callback) = self.array_start.as_mut() {
                callback(path, value.clone());
                let mut items = Vec::new();
                value.for_each_array_item(|item| items.push(item));
                for (index, item) in items.into_iter().enumerate() {
                    let mut item_path = path.to_vec();
                    item_path.extend_from_slice(format!("[{index}]").as_bytes());
                    self.array_item
                        .as_ref()
                        .expect("array item handler is paired with start")
                        .borrow_mut()
                        .handle(&item_path, item)?;
                }
                self.array_end
                    .as_mut()
                    .expect("array end handler is paired with start")(path);
                return Ok(());
            }
        }

        if let Some(handler) = &self.fallback {
            handler.borrow_mut().handle(path, value)?;
            return Ok(());
        }

        Err(unexpected_type(path))
    }
}

fn unexpected_key(key: &[u8], path: &[u8]) -> JsonHandlerError {
    JsonHandlerError(format!(
        "JSON handler found unexpected key {} in object at {}",
        String::from_utf8_lossy(key),
        String::from_utf8_lossy(path)
    ))
}

fn unexpected_type(path: &[u8]) -> JsonHandlerError {
    JsonHandlerError(format!(
        "JSON handler: value at {} is not of expected type",
        String::from_utf8_lossy(path)
    ))
}
