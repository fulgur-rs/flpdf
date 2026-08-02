//! qpdf-native canonical object cache and lazy resolver.
//!
//! This module is deliberately independent of the raw `Object` cache and
//! every `qpdf-cutover-delete(flpdf-25kg.3.3)` route.

use crate::object_handle::{DocumentResolver, ObjectValue, NO_PARSED_OFFSET};
use crate::parser::{parse_qpdf_direct_object_handle, HandleResolver};
use crate::reader::SharedInput;
use crate::tokenizer::Tokenizer;
use crate::{Error, ObjectHandle, ObjectRef, Result, XrefEntry};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::rc::Weak;

pub(crate) struct QpdfResolver<R: Read + Seek> {
    input: SharedInput<R>,
    header_offset: usize,
    xref: BTreeMap<ObjectRef, XrefEntry>,
    object_offsets: Vec<u64>,
    handles: RefCell<BTreeMap<ObjectRef, ObjectHandle>>,
    resolving: RefCell<BTreeSet<ObjectRef>>,
    self_link: RefCell<Option<Weak<dyn DocumentResolver>>>,
}

impl<R: Read + Seek> QpdfResolver<R> {
    pub(crate) fn new(
        input: SharedInput<R>,
        header_offset: usize,
        xref: BTreeMap<ObjectRef, XrefEntry>,
        object_offsets: Vec<u64>,
    ) -> Self {
        Self {
            input,
            header_offset,
            xref,
            object_offsets,
            handles: RefCell::new(BTreeMap::new()),
            resolving: RefCell::new(BTreeSet::new()),
            self_link: RefCell::new(None),
        }
    }

    pub(crate) fn attach(&self, resolver: Weak<dyn DocumentResolver>) {
        *self.self_link.borrow_mut() = Some(resolver);
    }

    pub(crate) fn get_object(&self, object_ref: ObjectRef) -> ObjectHandle {
        if let Some(handle) = self.handles.borrow().get(&object_ref) {
            return handle.clone();
        }
        let resolver = self
            .self_link
            .borrow()
            .clone()
            .expect("QpdfResolver is attached before public lookup");
        let handle =
            ObjectHandle::new_indirect_with_resolver(object_ref, NO_PARSED_OFFSET, resolver);
        self.handles.borrow_mut().insert(object_ref, handle.clone());
        handle
    }

    pub(crate) fn disconnect_all(&self) {
        for handle in self.handles.borrow().values() {
            handle.disconnect();
        }
        self.self_link.borrow_mut().take();
    }

    fn next_offset(&self, offset: u64) -> Option<u64> {
        self.object_offsets
            .iter()
            .copied()
            .find(|candidate| *candidate > offset)
    }

    fn read_object_window(&self, offset: u64) -> Result<Vec<u8>> {
        let physical_offset = u64::try_from(self.header_offset)
            .unwrap_or(u64::MAX)
            .saturating_add(offset);
        let mut input = self.input.clone();
        input.seek(SeekFrom::Start(physical_offset))?;
        let mut bytes = Vec::new();
        match self.next_offset(offset) {
            Some(next) => {
                input
                    .take(next.saturating_sub(offset))
                    .read_to_end(&mut bytes)?;
            }
            None => {
                input.read_to_end(&mut bytes)?;
            }
        }
        Ok(bytes)
    }

    fn parse_uncompressed(&self, object_ref: ObjectRef, offset: u64) -> Result<(ObjectValue, i64)> {
        let bytes = self.read_object_window(offset)?;
        let mut tokenizer = Tokenizer::new(&bytes);
        let number = tokenizer.next_integer()?;
        let generation = tokenizer.next_integer()?;
        if number != i64::from(object_ref.number) || generation != i64::from(object_ref.generation)
        {
            return Err(Error::parse(
                0,
                "xref entry points to the wrong indirect object",
            ));
        }
        tokenizer.expect_word(b"obj")?;
        tokenizer.skip_ignorable()?;
        let body_start = tokenizer.position();
        let base_offset = i64::try_from(offset).unwrap_or(i64::MAX) + body_start as i64;
        let mut resolver = ParserResolver { resolver: self };
        parse_qpdf_direct_object_handle(&bytes[body_start..], base_offset, &mut resolver)
            .map_err(|error| error.rebase_offset(body_start))
    }
}

struct ParserResolver<'a, R: Read + Seek> {
    resolver: &'a QpdfResolver<R>,
}

impl<R: Read + Seek> HandleResolver for ParserResolver<'_, R> {
    fn indirect_handle(&mut self, object_ref: ObjectRef) -> ObjectHandle {
        self.resolver.get_object(object_ref)
    }
}

impl<R: Read + Seek> DocumentResolver for QpdfResolver<R> {
    fn resolve_indirect(&self, object_ref: ObjectRef, handle: &ObjectHandle) -> Result<()> {
        if !self.resolving.borrow_mut().insert(object_ref) {
            handle.set_missing();
            return Ok(());
        }

        let result = match self.xref.get(&object_ref) {
            Some(XrefEntry::Uncompressed { offset }) => self
                .parse_uncompressed(object_ref, *offset)
                .map(|(value, parsed_offset)| {
                    handle.set_parsed_offset_if_unset(parsed_offset);
                    handle.set_resolved(value);
                }),
            Some(XrefEntry::Compressed { .. }) => Err(Error::Unsupported(
                "qpdf-native compressed-object resolution is not implemented".to_string(),
            )),
            Some(XrefEntry::Free { .. }) | None => {
                handle.set_missing();
                Ok(())
            }
        };
        self.resolving.borrow_mut().remove(&object_ref);
        result
    }
}
