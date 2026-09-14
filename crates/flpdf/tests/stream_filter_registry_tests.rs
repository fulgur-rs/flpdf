use flpdf::pipeline::{Pipeline, PipelineError, PipelineRef};
use flpdf::{
    register_stream_filter, DecodeLevel, Error, ObjectHandle, OwnedDecodePipeline, Result,
    StreamFilter,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct PrefixPipeline<'a> {
    next: PipelineRef<'a>,
    prefix: &'static [u8],
}

impl Pipeline for PrefixPipeline<'_> {
    fn identifier(&self) -> &str {
        "registry prefix"
    }

    fn write(&mut self, data: &[u8]) -> std::result::Result<(), PipelineError> {
        self.next.write(self.prefix)?;
        self.next.write(data)
    }

    fn finish(&mut self) -> std::result::Result<(), PipelineError> {
        self.next.finish()
    }
}

struct PrefixFilter {
    prefix: &'static [u8],
}

impl StreamFilter for PrefixFilter {
    fn get_decode_pipeline<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(PrefixPipeline {
            next,
            prefix: self.prefix,
        })))
    }
}

struct PassFilter;

impl StreamFilter for PassFilter {
    fn get_decode_pipeline<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::NoStage(next))
    }
}

struct ReRegisterOnDrop;

impl Drop for ReRegisterOnDrop {
    fn drop(&mut self) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            register_stream_filter(b"/FlpdfRegistryReentrantDrop", || Ok(PassFilter));
            sender.send(()).unwrap();
        });
        assert!(
            receiver.recv_timeout(Duration::from_secs(2)).is_ok(),
            "factory capture was dropped while the registry mutex was held"
        );
        handle.join().unwrap();
    }
}

struct DecodeParmsFilter {
    marker: Arc<Mutex<Option<i64>>>,
}

impl StreamFilter for DecodeParmsFilter {
    fn set_decode_params(&mut self, decode_params: &ObjectHandle) -> Result<bool> {
        *self.marker.lock().unwrap() = decode_params.try_get_key(b"/Marker")?.as_integer();
        Ok(true)
    }

    fn get_decode_pipeline<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::NoStage(next))
    }
}

struct FailingFilter;

impl StreamFilter for FailingFilter {
    fn get_decode_pipeline<'a>(
        &mut self,
        _next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Err(Error::Unsupported("filter callback failure".to_owned()))
    }
}

struct DropPipeline<'a> {
    next: PipelineRef<'a>,
    drops: Arc<AtomicUsize>,
}

impl Drop for DropPipeline<'_> {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl Pipeline for DropPipeline<'_> {
    fn identifier(&self) -> &str {
        "registry drop"
    }

    fn write(&mut self, data: &[u8]) -> std::result::Result<(), PipelineError> {
        self.next.write(data)
    }

    fn finish(&mut self) -> std::result::Result<(), PipelineError> {
        self.next.finish()
    }
}

struct DropFilter {
    filter_drops: Arc<AtomicUsize>,
    pipeline_drops: Arc<AtomicUsize>,
}

impl Drop for DropFilter {
    fn drop(&mut self) {
        self.filter_drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl StreamFilter for DropFilter {
    fn get_decode_pipeline<'a>(
        &mut self,
        next: PipelineRef<'a>,
    ) -> Result<OwnedDecodePipeline<'a>> {
        Ok(OwnedDecodePipeline::Stage(Box::new(DropPipeline {
            next,
            drops: Arc::clone(&self.pipeline_drops),
        })))
    }
}

fn filtered_stream(filter: ObjectHandle, decode_parms: Option<ObjectHandle>) -> ObjectHandle {
    let mut entries = vec![(b"Filter".to_vec(), filter)];
    if let Some(decode_parms) = decode_parms {
        entries.push((b"DecodeParms".to_vec(), decode_parms));
    }
    ObjectHandle::stream(
        ObjectHandle::dictionary(entries),
        std::rc::Rc::new(b"payload".to_vec()),
    )
}

#[test]
fn registered_filter_is_used_by_the_canonical_object_handle_decoder() {
    register_stream_filter(b"/FlpdfRegistryPrefix", || {
        Ok(PrefixFilter { prefix: b"custom:" })
    });

    let stream = filtered_stream(ObjectHandle::name(b"FlpdfRegistryPrefix".to_vec()), None);

    assert_eq!(
        stream
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"custom:payload"
    );
}

#[test]
fn later_registration_replaces_an_existing_factory() {
    let name = b"/FlpdfRegistryReplacement";
    register_stream_filter(name, || Ok(PrefixFilter { prefix: b"first:" }));
    register_stream_filter(name, || Ok(PrefixFilter { prefix: b"second:" }));

    let stream = filtered_stream(ObjectHandle::name(name[1..].to_vec()), None);
    assert_eq!(
        stream
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"second:payload"
    );
}

#[test]
fn escaped_slash_in_a_pdf_name_keeps_a_distinct_registry_key() {
    let name = b"FlpdfRegistrySlashKey";
    register_stream_filter(b"/FlpdfRegistrySlashKey", || {
        Ok(PrefixFilter {
            prefix: b"ordinary:",
        })
    });
    register_stream_filter(b"//FlpdfRegistrySlashKey", || {
        Ok(PrefixFilter {
            prefix: b"escaped:",
        })
    });

    let ordinary = filtered_stream(ObjectHandle::name(name.to_vec()), None);
    let mut escaped_name = vec![b'/'];
    escaped_name.extend_from_slice(name);
    let escaped = filtered_stream(ObjectHandle::name(escaped_name), None);
    assert_eq!(
        ordinary
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"ordinary:payload"
    );
    assert_eq!(
        escaped
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"escaped:payload"
    );
}

#[test]
fn replacing_a_factory_drops_the_old_capture_after_unlocking_the_registry() {
    let capture = ReRegisterOnDrop;
    register_stream_filter(b"/FlpdfRegistryDropOld", move || {
        let _ = &capture;
        Ok(PassFilter)
    });
    register_stream_filter(b"/FlpdfRegistryDropOld", || Ok(PassFilter));
}

#[test]
fn custom_filter_receives_the_full_decode_params_handle() {
    let marker = Arc::new(Mutex::new(None));
    let factory_marker = Arc::clone(&marker);
    register_stream_filter(b"/FlpdfRegistryDecodeParms", move || {
        Ok(DecodeParmsFilter {
            marker: Arc::clone(&factory_marker),
        })
    });

    let stream = filtered_stream(
        ObjectHandle::name(b"FlpdfRegistryDecodeParms".to_vec()),
        Some(ObjectHandle::dictionary(vec![(
            b"/Marker".to_vec(),
            ObjectHandle::integer(7),
        )])),
    );

    assert_eq!(
        stream
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"payload"
    );
    assert_eq!(*marker.lock().unwrap(), Some(7));
}

#[test]
fn unknown_filter_does_not_skip_later_known_factory_construction() {
    let constructed = Arc::new(Mutex::new(Vec::new()));
    let first_constructed = Arc::clone(&constructed);
    register_stream_filter(b"/FlpdfRegistryKnownBeforeUnknown", move || {
        first_constructed.lock().unwrap().push("first");
        Ok(PassFilter)
    });
    let last_constructed = Arc::clone(&constructed);
    register_stream_filter(b"/FlpdfRegistryKnownAfterUnknown", move || {
        last_constructed.lock().unwrap().push("last");
        Ok(PassFilter)
    });

    let stream = filtered_stream(
        ObjectHandle::array(vec![
            ObjectHandle::name(b"FlpdfRegistryKnownBeforeUnknown".to_vec()),
            ObjectHandle::name(b"FlpdfRegistryUnknown".to_vec()),
            ObjectHandle::name(b"FlpdfRegistryKnownAfterUnknown".to_vec()),
        ]),
        Some(ObjectHandle::integer(1)),
    );

    assert!(!stream
        .stream_data_filterable(DecodeLevel::Generalized)
        .unwrap());
    assert_eq!(*constructed.lock().unwrap(), vec!["first", "last"]);
}

#[test]
fn factory_errors_propagate_before_decode_params_are_inspected() {
    register_stream_filter(b"/FlpdfRegistryFactoryError", || -> Result<PassFilter> {
        Err(Error::Unsupported("factory failure".to_owned()))
    });

    let stream = filtered_stream(
        ObjectHandle::name(b"FlpdfRegistryFactoryError".to_vec()),
        Some(ObjectHandle::array(vec![])),
    );
    let error = stream
        .stream_data_filterable(DecodeLevel::Generalized)
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported(message) if message == "factory failure"));
}

#[test]
fn filter_callback_errors_propagate_through_canonical_decode() {
    register_stream_filter(b"/FlpdfRegistryFilterError", || Ok(FailingFilter));

    let stream = filtered_stream(
        ObjectHandle::name(b"FlpdfRegistryFilterError".to_vec()),
        None,
    );
    let error = stream
        .get_stream_data(DecodeLevel::Generalized)
        .unwrap_err();
    assert!(matches!(error, Error::Unsupported(message) if message == "filter callback failure"));
}

#[test]
fn registered_filter_and_pipeline_are_dropped_after_canonical_decode() {
    let filter_drops = Arc::new(AtomicUsize::new(0));
    let pipeline_drops = Arc::new(AtomicUsize::new(0));
    let factory_filter_drops = Arc::clone(&filter_drops);
    let factory_pipeline_drops = Arc::clone(&pipeline_drops);
    register_stream_filter(b"/FlpdfRegistryDrop", move || {
        Ok(DropFilter {
            filter_drops: Arc::clone(&factory_filter_drops),
            pipeline_drops: Arc::clone(&factory_pipeline_drops),
        })
    });

    let stream = filtered_stream(ObjectHandle::name(b"FlpdfRegistryDrop".to_vec()), None);
    assert_eq!(
        stream
            .get_stream_data(DecodeLevel::Generalized)
            .unwrap()
            .as_slice(),
        b"payload"
    );
    assert_eq!(filter_drops.load(Ordering::SeqCst), 1);
    assert_eq!(pipeline_drops.load(Ordering::SeqCst), 1);
}
