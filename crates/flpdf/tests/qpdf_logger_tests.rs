use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{Error, QPDFLogger};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct SinkState {
    bytes: Vec<u8>,
    finishes: usize,
}

struct RecordingSink {
    identifier: &'static str,
    state: Arc<Mutex<SinkState>>,
}

impl Pipeline for RecordingSink {
    fn identifier(&self) -> &str {
        self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.state.lock().unwrap().bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.state.lock().unwrap().finishes += 1;
        Ok(())
    }
}

fn recording(identifier: &'static str) -> (PipelineHandle, Arc<Mutex<SinkState>>) {
    let state = Arc::new(Mutex::new(SinkState::default()));
    let handle = PipelineHandle::new(RecordingSink {
        identifier,
        state: Arc::clone(&state),
    });
    (handle, state)
}

#[test]
fn default_routes_and_unset_save_match_qpdf() {
    let logger = QPDFLogger::create();

    assert!(logger
        .get_info()
        .unwrap()
        .is_same(&logger.standard_output()));
    assert!(logger.get_warn().unwrap().is_same(&logger.standard_error()));
    assert!(logger
        .get_error()
        .unwrap()
        .is_same(&logger.standard_error()));
    assert!(logger.get_save_if_set().is_none());

    let error = logger.get_save().unwrap_err();
    assert!(matches!(
        error,
        Error::Internal(ref message)
            if message == "QPDFLogger: requested a null pipeline without null_okay == true"
    ));
}

#[test]
fn default_routes_deliver_info_warn_and_error_to_the_selected_sinks() {
    let logger = QPDFLogger::create();
    let (output, output_state) = recording("output");
    let (error, error_state) = recording("error");
    logger.set_output_streams(Some(output), Some(error));

    logger.info(b"info\n").unwrap();
    logger.warn(b"warn\n").unwrap();
    logger.error(b"error\n").unwrap();

    assert_eq!(output_state.lock().unwrap().bytes, b"info\n");
    assert_eq!(error_state.lock().unwrap().bytes, b"warn\nerror\n");
}

#[test]
fn warn_follows_error_until_it_is_independently_assigned() {
    let logger = QPDFLogger::create();
    let (errors, errors_state) = recording("errors");
    let (warnings, warnings_state) = recording("warnings");
    let (errors_two, errors_two_state) = recording("errors-two");

    logger.set_error(Some(errors));
    logger.warn(b"warn follows error\n").unwrap();
    logger.error(b"error too\n").unwrap();
    logger.set_warn(Some(warnings));
    logger.warn(b"warning now separate\n").unwrap();
    logger.set_error(Some(errors_two));
    logger.warn(b"still separate\n").unwrap();
    logger.error(b"new error\n").unwrap();
    logger.set_warn(None);
    logger.warn(b"following again\n").unwrap();

    assert_eq!(
        errors_state.lock().unwrap().bytes,
        b"warn follows error\nerror too\n"
    );
    assert_eq!(
        warnings_state.lock().unwrap().bytes,
        b"warning now separate\nstill separate\n"
    );
    assert_eq!(
        errors_two_state.lock().unwrap().bytes,
        b"new error\nfollowing again\n"
    );
}

#[test]
fn reset_and_set_output_streams_restore_qpdf_route_relationships() {
    let logger = QPDFLogger::create();
    let (output, output_state) = recording("output");
    let (errors, errors_state) = recording("errors");
    let (separate_warn, separate_warn_state) = recording("separate-warn");

    logger.set_warn(Some(separate_warn));
    logger.set_output_streams(Some(output), Some(errors));
    logger.info(b"info\n").unwrap();
    logger.warn(b"warn follows reset error\n").unwrap();
    logger.error(b"error\n").unwrap();
    logger.set_info(Some(logger.discard()));
    logger.info(b"discarded\n").unwrap();

    assert_eq!(output_state.lock().unwrap().bytes, b"info\n");
    assert_eq!(
        errors_state.lock().unwrap().bytes,
        b"warn follows reset error\nerror\n"
    );
    assert!(separate_warn_state.lock().unwrap().bytes.is_empty());
}

#[test]
fn save_first_reroutes_and_later_restores_default_info() {
    let logger = QPDFLogger::create();

    logger.save_to_standard_output(true).unwrap();
    assert!(logger
        .get_save()
        .unwrap()
        .is_same(&logger.standard_output()));
    assert!(logger.get_info().unwrap().is_same(&logger.standard_error()));

    logger.set_info(None);
    assert!(logger.get_info().unwrap().is_same(&logger.standard_error()));

    logger.set_save(None, false).unwrap();
    logger.set_info(None);
    assert!(logger
        .get_info()
        .unwrap()
        .is_same(&logger.standard_output()));
}

#[test]
fn stdout_use_collision_is_internal_even_for_an_empty_write() {
    let logger = QPDFLogger::create();
    logger.info(b"").unwrap();

    let error = logger.save_to_standard_output(false).unwrap_err();
    assert!(matches!(
        error,
        Error::Internal(ref message)
            if message == "QPDFLogger: called setSave on standard output after standard output has already been used"
    ));
}

#[test]
fn same_save_and_only_if_not_set_short_circuit_stdout_collision() {
    let same = QPDFLogger::create();
    same.save_to_standard_output(false).unwrap();
    let stdout = same.standard_output();
    same.get_save().unwrap().write(b"").unwrap();
    same.set_save(Some(stdout), false).unwrap();

    let only_if = QPDFLogger::create();
    let (custom, _) = recording("custom-save");
    only_if.set_save(Some(custom.clone()), false).unwrap();
    only_if.info(b"").unwrap();
    only_if.save_to_standard_output(true).unwrap();
    assert!(only_if.get_save().unwrap().is_same(&custom));
}

struct FailingSink {
    error: Option<PipelineError>,
}

#[test]
fn logger_translates_downstream_pipeline_error_categories() {
    let logic_logger = QPDFLogger::create();
    logic_logger.set_info(Some(PipelineHandle::new(FailingSink {
        error: Some(PipelineError::logic("logic detail")),
    })));
    let runtime_logger = QPDFLogger::create();
    runtime_logger.set_info(Some(PipelineHandle::new(FailingSink {
        error: Some(PipelineError::runtime("runtime detail")),
    })));

    assert!(matches!(
        logic_logger.info(b"x"),
        Err(Error::Internal(ref message)) if message == "logic detail"
    ));
    assert!(matches!(
        runtime_logger.info(b"x"),
        Err(Error::System(ref message)) if message == "runtime detail"
    ));
}

#[test]
fn dropping_logger_does_not_finish_a_custom_sink() {
    let (custom, state) = recording("custom");
    {
        let logger = QPDFLogger::create();
        logger.set_info(Some(custom.clone()));
        logger.info(b"payload").unwrap();
    }

    assert_eq!(state.lock().unwrap().finishes, 0);
    custom.finish().unwrap();
    assert_eq!(state.lock().unwrap().finishes, 1);
}

impl Pipeline for FailingSink {
    fn identifier(&self) -> &str {
        "failing"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Err(self.error.take().expect("one test write"))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}
