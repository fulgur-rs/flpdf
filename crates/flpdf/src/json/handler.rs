//! qpdf correspondence: JSONHandler.cc recursive dispatch responsibilities with Rust shared ownership.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::rc::{Rc, Weak};

use super::{Json, JsonMessage};

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("{0}")]
pub struct JsonHandlerError(pub JsonMessage);

type JsonCallback = Rc<dyn Fn(&[u8], Json)>;
type BytesCallback = Rc<dyn Fn(&[u8], &[u8])>;
type PathCallback = Rc<dyn Fn(&[u8])>;
type BoolCallback = Rc<dyn Fn(&[u8], bool)>;

#[derive(Clone)]
enum HandlerEdge {
    Strong(JsonHandler),
    Weak(WeakJsonHandler),
}

impl HandlerEdge {
    fn resolve(&self) -> Option<JsonHandler> {
        match self {
            Self::Strong(handler) => Some(handler.clone()),
            Self::Weak(handler) => handler.upgrade(),
        }
    }

    fn strong_target(&self) -> Option<JsonHandler> {
        match self {
            Self::Strong(handler) => Some(handler.clone()),
            Self::Weak(_) => None,
        }
    }
}

#[derive(Default)]
struct Handlers {
    any: Option<JsonCallback>,
    null: Option<PathCallback>,
    string: Option<BytesCallback>,
    number: Option<BytesCallback>,
    boolean: Option<BoolCallback>,
    dictionary_start: Option<JsonCallback>,
    dictionary_end: Option<PathCallback>,
    array_start: Option<JsonCallback>,
    array_end: Option<PathCallback>,
    dictionary_keys: BTreeMap<Vec<u8>, HandlerEdge>,
    fallback_dictionary: Option<HandlerEdge>,
    array_item: Option<HandlerEdge>,
    fallback: Option<HandlerEdge>,
}

#[derive(Clone, Default)]
pub struct JsonHandler {
    inner: Rc<RefCell<Handlers>>,
}

#[derive(Clone)]
pub struct WeakJsonHandler {
    inner: Weak<RefCell<Handlers>>,
}

impl JsonHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn downgrade(&self) -> WeakJsonHandler {
        WeakJsonHandler {
            inner: Rc::downgrade(&self.inner),
        }
    }

    pub fn add_any_handler(&self, callback: impl Fn(&[u8], Json) + 'static) {
        self.inner.borrow_mut().any = Some(Rc::new(callback));
    }

    pub fn add_null_handler(&self, callback: impl Fn(&[u8]) + 'static) {
        self.inner.borrow_mut().null = Some(Rc::new(callback));
    }

    pub fn add_string_handler(&self, callback: impl Fn(&[u8], &[u8]) + 'static) {
        self.inner.borrow_mut().string = Some(Rc::new(callback));
    }

    pub fn add_number_handler(&self, callback: impl Fn(&[u8], &[u8]) + 'static) {
        self.inner.borrow_mut().number = Some(Rc::new(callback));
    }

    pub fn add_bool_handler(&self, callback: impl Fn(&[u8], bool) + 'static) {
        self.inner.borrow_mut().boolean = Some(Rc::new(callback));
    }

    pub fn add_dictionary_handlers(
        &self,
        start: impl Fn(&[u8], Json) + 'static,
        end: impl Fn(&[u8]) + 'static,
    ) {
        let mut handlers = self.inner.borrow_mut();
        handlers.dictionary_start = Some(Rc::new(start));
        handlers.dictionary_end = Some(Rc::new(end));
    }

    pub fn add_dictionary_key_handler(&self, key: impl AsRef<[u8]>, handler: JsonHandler) {
        let edge = self.edge_to(handler);
        self.inner
            .borrow_mut()
            .dictionary_keys
            .insert(key.as_ref().to_vec(), edge);
    }

    pub fn add_fallback_dictionary_handler(&self, handler: JsonHandler) {
        let edge = self.edge_to(handler);
        self.inner.borrow_mut().fallback_dictionary = Some(edge);
    }

    pub fn add_array_handlers(
        &self,
        start: impl Fn(&[u8], Json) + 'static,
        end: impl Fn(&[u8]) + 'static,
        item: JsonHandler,
    ) {
        let item = self.edge_to(item);
        let mut handlers = self.inner.borrow_mut();
        handlers.array_start = Some(Rc::new(start));
        handlers.array_end = Some(Rc::new(end));
        handlers.array_item = Some(item);
    }

    pub fn add_fallback_handler(&self, handler: JsonHandler) {
        let edge = self.edge_to(handler);
        self.inner.borrow_mut().fallback = Some(edge);
    }

    pub fn handle(&self, path: &[u8], value: Json) -> Result<(), JsonHandlerError> {
        let callback = { self.inner.borrow().any.clone() };
        if let Some(callback) = callback {
            callback(path, value);
            return Ok(());
        }

        if value.is_null() {
            let callback = { self.inner.borrow().null.clone() };
            if let Some(callback) = callback {
                callback(path);
                return Ok(());
            }
        }
        if let Some(string) = value.get_string() {
            let callback = { self.inner.borrow().string.clone() };
            if let Some(callback) = callback {
                callback(path, &string);
                return Ok(());
            }
        }
        if let Some(number) = value.get_number() {
            let callback = { self.inner.borrow().number.clone() };
            if let Some(callback) = callback {
                callback(path, &number);
                return Ok(());
            }
        }
        if let Some(boolean) = value.get_bool() {
            let callback = { self.inner.borrow().boolean.clone() };
            if let Some(callback) = callback {
                callback(path, boolean);
                return Ok(());
            }
        }

        if value.is_dictionary() {
            let start = { self.inner.borrow().dictionary_start.clone() };
            if let Some(start) = start {
                start(path, value.clone());
                let mut path_base = path.to_vec();
                if path_base != b"." {
                    path_base.push(b'.');
                }
                let mut item_error = None;
                value.for_each_dict_item(|key, item| {
                    if item_error.is_some() {
                        return;
                    }
                    let target = {
                        let handlers = self.inner.borrow();
                        match handlers.dictionary_keys.get(key) {
                            Some(handler) => handler.resolve(),
                            None => handlers
                                .fallback_dictionary
                                .as_ref()
                                .and_then(HandlerEdge::resolve),
                        }
                    };
                    let mut item_path = path_base.clone();
                    item_path.extend_from_slice(key);
                    item_error = match target {
                        Some(target) => target.handle(&item_path, item).err(),
                        None => Some(unexpected_key(key, path)),
                    };
                });
                if let Some(error) = item_error {
                    return Err(error);
                }
                let end = self
                    .inner
                    .borrow()
                    .dictionary_end
                    .clone()
                    .expect("dictionary start and end handlers are registered together");
                end(path);
                return Ok(());
            }
        }

        if value.is_array() {
            let start = { self.inner.borrow().array_start.clone() };
            if let Some(start) = start {
                start(path, value.clone());
                let mut items = Vec::new();
                value.for_each_array_item(|item| items.push(item));
                for (index, item) in items.into_iter().enumerate() {
                    let mut item_path = path.to_vec();
                    item_path.extend_from_slice(format!("[{index}]").as_bytes());
                    let target = self
                        .inner
                        .borrow()
                        .array_item
                        .as_ref()
                        .and_then(HandlerEdge::resolve)
                        .ok_or_else(|| unexpected_type(&item_path))?;
                    target.handle(&item_path, item)?;
                }
                let end = self
                    .inner
                    .borrow()
                    .array_end
                    .clone()
                    .expect("array start and end handlers are registered together");
                end(path);
                return Ok(());
            }
        }

        let fallback = {
            self.inner
                .borrow()
                .fallback
                .as_ref()
                .and_then(HandlerEdge::resolve)
        };
        if let Some(fallback) = fallback {
            return fallback.handle(path, value);
        }

        Err(unexpected_type(path))
    }

    fn edge_to(&self, handler: JsonHandler) -> HandlerEdge {
        if Rc::ptr_eq(&self.inner, &handler.inner) {
            return HandlerEdge::Weak(handler.downgrade());
        }
        if handler.has_strong_path_to(&self.inner) {
            HandlerEdge::Weak(handler.downgrade())
        } else {
            HandlerEdge::Strong(handler)
        }
    }

    fn has_strong_path_to(&self, target: &Rc<RefCell<Handlers>>) -> bool {
        let mut pending = vec![self.clone()];
        let mut seen = HashSet::new();

        while let Some(handler) = pending.pop() {
            if !seen.insert(Rc::as_ptr(&handler.inner)) {
                continue;
            }
            for child in handler.strong_edges() {
                if Rc::ptr_eq(&child.inner, target) {
                    return true;
                } else {
                    pending.push(child);
                }
            }
        }

        false
    }

    fn strong_edges(&self) -> Vec<JsonHandler> {
        let handlers = self.inner.borrow();
        let mut edges = Vec::new();
        edges.extend(
            handlers
                .dictionary_keys
                .values()
                .filter_map(HandlerEdge::strong_target),
        );
        if let Some(handler) = handlers
            .fallback_dictionary
            .as_ref()
            .and_then(HandlerEdge::strong_target)
        {
            edges.push(handler);
        }
        if let Some(handler) = handlers
            .array_item
            .as_ref()
            .and_then(HandlerEdge::strong_target)
        {
            edges.push(handler);
        }
        if let Some(handler) = handlers
            .fallback
            .as_ref()
            .and_then(HandlerEdge::strong_target)
        {
            edges.push(handler);
        }
        edges
    }
}

impl WeakJsonHandler {
    pub fn upgrade(&self) -> Option<JsonHandler> {
        self.inner.upgrade().map(|inner| JsonHandler { inner })
    }
}

fn unexpected_key(key: &[u8], path: &[u8]) -> JsonHandlerError {
    let mut message = b"JSON handler found unexpected key ".to_vec();
    message.extend_from_slice(key);
    message.extend_from_slice(b" in object at ");
    message.extend_from_slice(path);
    JsonHandlerError(JsonMessage::from_bytes(message))
}

fn unexpected_type(path: &[u8]) -> JsonHandlerError {
    let mut message = b"JSON handler: value at ".to_vec();
    message.extend_from_slice(path);
    message.extend_from_slice(b" is not of expected type");
    JsonHandlerError(JsonMessage::from_bytes(message))
}
