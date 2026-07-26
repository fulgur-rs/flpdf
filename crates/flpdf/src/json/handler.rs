//! qpdf correspondence: JSONHandler.cc recursive dispatch responsibilities with Rust shared ownership.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::Json;

pub type SharedJsonHandler = Rc<RefCell<JsonHandler>>;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("{0}")]
pub struct JsonHandlerError(pub String);

type HandlerKey = usize;
type JsonCallback = Rc<RefCell<Box<dyn FnMut(&[u8], Json)>>>;
type BytesCallback = Rc<RefCell<Box<dyn FnMut(&[u8], &[u8])>>>;
type PathCallback = Rc<RefCell<Box<dyn FnMut(&[u8])>>>;
type BoolCallback = Rc<RefCell<Box<dyn FnMut(&[u8], bool)>>>;

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
    dictionary_keys: BTreeMap<Vec<u8>, HandlerTarget>,
    fallback_dictionary: Option<HandlerTarget>,
    array_item: Option<HandlerTarget>,
    fallback: Option<HandlerTarget>,
}

#[derive(Clone)]
struct HandlerTarget {
    key: HandlerKey,
    handler: SharedJsonHandler,
}

impl HandlerTarget {
    fn new(handler: SharedJsonHandler) -> Self {
        let key = handler.as_ref().as_ptr() as HandlerKey;
        Self { key, handler }
    }
}

#[derive(Clone)]
struct HandlerSnapshot {
    any: Option<JsonCallback>,
    null: Option<PathCallback>,
    string: Option<BytesCallback>,
    number: Option<BytesCallback>,
    boolean: Option<BoolCallback>,
    dictionary_start: Option<JsonCallback>,
    dictionary_end: Option<PathCallback>,
    array_start: Option<JsonCallback>,
    array_end: Option<PathCallback>,
    dictionary_keys: BTreeMap<Vec<u8>, HandlerTarget>,
    fallback_dictionary: Option<HandlerTarget>,
    array_item: Option<HandlerTarget>,
    fallback: Option<HandlerTarget>,
}

#[derive(Clone)]
struct ActiveHandler {
    snapshot: HandlerSnapshot,
    live: Option<SharedJsonHandler>,
}

#[derive(Default)]
struct DispatchContext {
    active: BTreeMap<HandlerKey, ActiveHandler>,
}

impl JsonHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shared() -> SharedJsonHandler {
        Rc::new(RefCell::new(Self::new()))
    }

    /// Handles a value without retaining the shared handler's borrow during callbacks.
    ///
    /// Use this entry point when a callback may dispatch through the same
    /// [`SharedJsonHandler`]. Reentering the same active `FnMut` callback is
    /// unsupported because its mutable callback state is already borrowed.
    /// A callback that needs to reenter this handler should capture
    /// `Rc::downgrade(handler)` and upgrade it for the nested call. Capturing a
    /// strong clone in a callback owned by the same handler creates the same
    /// ownership cycle as a qpdf `shared_ptr` callback capture.
    pub fn handle_shared(
        handler: &SharedJsonHandler,
        path: &[u8],
        value: Json,
    ) -> Result<(), JsonHandlerError> {
        let key = handler.as_ref().as_ptr() as HandlerKey;
        let snapshot = {
            let handler = handler.borrow();
            handler.snapshot()
        };
        let mut context = DispatchContext::default();
        context.active.insert(
            key,
            ActiveHandler {
                snapshot: snapshot.clone(),
                live: Some(handler.clone()),
            },
        );
        snapshot.handle(&mut context, key, path, value)
    }

    pub fn add_any_handler(&mut self, callback: impl FnMut(&[u8], Json) + 'static) {
        self.any = Some(Rc::new(RefCell::new(Box::new(callback))));
    }

    pub fn add_null_handler(&mut self, callback: impl FnMut(&[u8]) + 'static) {
        self.null = Some(Rc::new(RefCell::new(Box::new(callback))));
    }

    pub fn add_string_handler(&mut self, callback: impl FnMut(&[u8], &[u8]) + 'static) {
        self.string = Some(Rc::new(RefCell::new(Box::new(callback))));
    }

    pub fn add_number_handler(&mut self, callback: impl FnMut(&[u8], &[u8]) + 'static) {
        self.number = Some(Rc::new(RefCell::new(Box::new(callback))));
    }

    pub fn add_bool_handler(&mut self, callback: impl FnMut(&[u8], bool) + 'static) {
        self.boolean = Some(Rc::new(RefCell::new(Box::new(callback))));
    }

    pub fn add_dictionary_handlers(
        &mut self,
        start: impl FnMut(&[u8], Json) + 'static,
        end: impl FnMut(&[u8]) + 'static,
    ) {
        self.dictionary_start = Some(Rc::new(RefCell::new(Box::new(start))));
        self.dictionary_end = Some(Rc::new(RefCell::new(Box::new(end))));
    }

    pub fn add_dictionary_key_handler(
        &mut self,
        key: impl AsRef<[u8]>,
        handler: SharedJsonHandler,
    ) {
        let target = HandlerTarget::new(handler);
        self.dictionary_keys.insert(key.as_ref().to_vec(), target);
    }

    pub fn add_fallback_dictionary_handler(&mut self, handler: SharedJsonHandler) {
        self.fallback_dictionary = Some(HandlerTarget::new(handler));
    }

    pub fn add_array_handlers(
        &mut self,
        start: impl FnMut(&[u8], Json) + 'static,
        end: impl FnMut(&[u8]) + 'static,
        item: SharedJsonHandler,
    ) {
        self.array_start = Some(Rc::new(RefCell::new(Box::new(start))));
        self.array_end = Some(Rc::new(RefCell::new(Box::new(end))));
        self.array_item = Some(HandlerTarget::new(item));
    }

    pub fn add_fallback_handler(&mut self, handler: SharedJsonHandler) {
        self.fallback = Some(HandlerTarget::new(handler));
    }

    pub fn handle(&mut self, path: &[u8], value: Json) -> Result<(), JsonHandlerError> {
        let snapshot = self.snapshot();
        let mut context = DispatchContext::default();
        let key = self as *const JsonHandler as HandlerKey;
        context.active.insert(
            key,
            ActiveHandler {
                snapshot: snapshot.clone(),
                live: None,
            },
        );
        snapshot.handle(&mut context, key, path, value)
    }

    fn snapshot(&self) -> HandlerSnapshot {
        HandlerSnapshot {
            any: self.any.clone(),
            null: self.null.clone(),
            string: self.string.clone(),
            number: self.number.clone(),
            boolean: self.boolean.clone(),
            dictionary_start: self.dictionary_start.clone(),
            dictionary_end: self.dictionary_end.clone(),
            array_start: self.array_start.clone(),
            array_end: self.array_end.clone(),
            dictionary_keys: self.dictionary_keys.clone(),
            fallback_dictionary: self.fallback_dictionary.clone(),
            array_item: self.array_item.clone(),
            fallback: self.fallback.clone(),
        }
    }
}

impl HandlerSnapshot {
    fn handle(
        &self,
        context: &mut DispatchContext,
        owner_key: HandlerKey,
        path: &[u8],
        value: Json,
    ) -> Result<(), JsonHandlerError> {
        if let Some(callback) = &self.any {
            callback.borrow_mut()(path, value);
            return Ok(());
        }

        if value.is_null() {
            if let Some(callback) = &self.null {
                callback.borrow_mut()(path);
                return Ok(());
            }
        }

        if let (Some(callback), Some(string)) = (&self.string, value.get_string()) {
            callback.borrow_mut()(path, &string);
            return Ok(());
        }

        if let (Some(callback), Some(number)) = (&self.number, value.get_number()) {
            callback.borrow_mut()(path, &number);
            return Ok(());
        }

        if let (Some(callback), Some(boolean)) = (&self.boolean, value.get_bool()) {
            callback.borrow_mut()(path, boolean);
            return Ok(());
        }

        if value.is_dictionary() {
            if let Some(callback) = &self.dictionary_start {
                callback.borrow_mut()(path, value.clone());
                let mut path_base = path.to_vec();
                if path_base != b"." {
                    path_base.push(b'.');
                }
                let mut item_error = None;
                value.for_each_dict_item(|key, item| {
                    if item_error.is_some() {
                        return;
                    }
                    let mut item_path = path_base.clone();
                    item_path.extend_from_slice(key);
                    let target = context
                        .active
                        .get(&owner_key)
                        .and_then(|active| active.live.as_ref())
                        .map(|live| {
                            let live = live.borrow();
                            live.dictionary_keys
                                .get(key)
                                .cloned()
                                .or_else(|| live.fallback_dictionary.clone())
                        })
                        .unwrap_or_else(|| {
                            self.dictionary_keys
                                .get(key)
                                .cloned()
                                .or_else(|| self.fallback_dictionary.clone())
                        });
                    item_error = if let Some(handler) = target {
                        context.dispatch(&handler, &item_path, item).err()
                    } else {
                        Some(unexpected_key(key, path))
                    };
                });
                if let Some(error) = item_error {
                    return Err(error);
                }
                context
                    .active
                    .get(&owner_key)
                    .and_then(|active| active.live.as_ref())
                    .map(|live| live.borrow().dictionary_end.clone())
                    .unwrap_or_else(|| self.dictionary_end.clone())
                    .as_ref()
                    .expect("dictionary end handler is paired with start")
                    .borrow_mut()(path);
                return Ok(());
            }
        }

        if value.is_array() {
            if let Some(callback) = &self.array_start {
                callback.borrow_mut()(path, value.clone());
                let mut items = Vec::new();
                value.for_each_array_item(|item| items.push(item));
                for (index, item) in items.into_iter().enumerate() {
                    let mut item_path = path.to_vec();
                    item_path.extend_from_slice(format!("[{index}]").as_bytes());
                    context
                        .active
                        .get(&owner_key)
                        .and_then(|active| active.live.as_ref())
                        .map(|live| live.borrow().array_item.clone())
                        .unwrap_or_else(|| self.array_item.clone())
                        .as_ref()
                        .expect("array item handler is paired with start")
                        .dispatch(context, &item_path, item)?;
                }
                context
                    .active
                    .get(&owner_key)
                    .and_then(|active| active.live.as_ref())
                    .map(|live| live.borrow().array_end.clone())
                    .unwrap_or_else(|| self.array_end.clone())
                    .as_ref()
                    .expect("array end handler is paired with start")
                    .borrow_mut()(path);
                return Ok(());
            }
        }

        if let Some(handler) = &self.fallback {
            context.dispatch(handler, path, value)?;
            return Ok(());
        }

        Err(unexpected_type(path))
    }
}

impl HandlerTarget {
    fn dispatch(
        &self,
        context: &mut DispatchContext,
        path: &[u8],
        value: Json,
    ) -> Result<(), JsonHandlerError> {
        context.dispatch(self, path, value)
    }
}

impl DispatchContext {
    fn dispatch(
        &mut self,
        target: &HandlerTarget,
        path: &[u8],
        value: Json,
    ) -> Result<(), JsonHandlerError> {
        if let Some(snapshot) = self.active.get(&target.key).map(|active| {
            active
                .live
                .as_ref()
                .map(|live| live.borrow().snapshot())
                .unwrap_or_else(|| active.snapshot.clone())
        }) {
            return snapshot.handle(self, target.key, path, value);
        }

        let handler = target.handler.clone();
        let snapshot = handler.borrow().snapshot();
        self.active.insert(
            target.key,
            ActiveHandler {
                snapshot: snapshot.clone(),
                live: Some(handler),
            },
        );
        let result = snapshot.handle(self, target.key, path, value);
        self.active.remove(&target.key);
        result
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
