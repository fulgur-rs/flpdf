//! qpdf correspondence: libqpdf/Pl_MD5.cc:5-65 and libqpdf/qpdf/Pl_MD5.hh:4-33 — unchanged forwarding, enable/persist state, reusable finish lifecycle, and hexadecimal digest retrieval.

use super::{Pipeline, PipelineError, PipelineResult};
use md5::{Digest, Md5};

const MAX_UPDATE_BYTES: usize = 1 << 30;

pub(crate) struct PlMd5<'a> {
    identifier: String,
    next: &'a mut dyn Pipeline,
    in_progress: bool,
    md5: Md5,
    enabled: bool,
    persist_across_finish: bool,
}

impl<'a> PlMd5<'a> {
    pub(crate) fn new(identifier: impl Into<String>, next: &'a mut dyn Pipeline) -> Self {
        Self {
            identifier: identifier.into(),
            next,
            in_progress: false,
            md5: Md5::new(),
            enabled: true,
            persist_across_finish: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    #[cfg(test)]
    pub(crate) fn persist_across_finish(&mut self, persist: bool) {
        self.persist_across_finish = persist;
    }

    pub(crate) fn get_hex_digest(&mut self) -> PipelineResult<String> {
        if !self.enabled {
            return Err(PipelineError::logic(
                "digest requested for a disabled MD5 Pipeline",
            ));
        }
        self.in_progress = false;
        Ok(hex::encode(self.md5.clone().finalize()))
    }
}

impl Pipeline for PlMd5<'_> {
    fn identifier(&self) -> &str {
        &self.identifier
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        if self.enabled {
            if !self.in_progress {
                self.md5 = Md5::new();
                self.in_progress = true;
            }
            for chunk in data.chunks(MAX_UPDATE_BYTES) {
                self.md5.update(chunk);
            }
        }
        self.next.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.next.finish()?;
        if !self.persist_across_finish {
            self.in_progress = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PlMd5;
    use crate::pipeline::test_support::{shared_trace, RecordingSink, TraceCall};
    use crate::pipeline::{Pipeline, PipelineError};

    #[test]
    fn preserves_the_pipeline_identifier() {
        let mut sink = RecordingSink::new(&[], &[]);
        let md5 = PlMd5::new("embedded-file-md5", &mut sink);
        let pipeline: &dyn Pipeline = &md5;

        assert_eq!(pipeline.identifier(), "embedded-file-md5");
    }

    #[test]
    fn forwards_original_chunks_and_reports_the_known_digest() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let digest = {
            let mut md5 = PlMd5::new("md5", &mut sink);
            md5.write(b"ab").unwrap();
            md5.write(b"c").unwrap();
            md5.finish().unwrap();
            md5.get_hex_digest().unwrap()
        };

        assert_eq!(digest, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(trace.borrow().output, b"abc");
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: b"ab".to_vec(),
                    failed: false,
                },
                TraceCall::Write {
                    data: b"c".to_vec(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }

    #[test]
    fn successful_finish_makes_the_next_write_start_a_new_digest() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"a").unwrap();
        md5.finish().unwrap();
        md5.write(b"b").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "92eb5ffee6ae2fec3ad71c777531578f"
        );
    }

    #[test]
    fn persistent_mode_accumulates_across_finish_boundaries() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.persist_across_finish(true);
        md5.write(b"a").unwrap();
        md5.finish().unwrap();
        md5.write(b"b").unwrap();
        md5.finish().unwrap();
        md5.write(b"c").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    #[test]
    fn repeated_digest_is_stable_and_a_later_write_resets() {
        let mut sink = RecordingSink::new(&[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"abc").unwrap();
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        md5.write(b"a").unwrap();
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "0cc175b9c0f1b6a831c399e269772661"
        );
    }

    #[test]
    fn disabled_mode_forwards_but_rejects_digest_without_losing_progress() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"a").unwrap();
        md5.enable(false);
        md5.write(b"b").unwrap();
        assert!(matches!(
            md5.get_hex_digest().unwrap_err(),
            PipelineError::Logic(_)
        ));
        assert_eq!(
            md5.get_hex_digest().unwrap_err().to_string(),
            "digest requested for a disabled MD5 Pipeline"
        );
        md5.enable(true);
        md5.write(b"c").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "e2075474294983e013ee4dd2201c7a73"
        );
        assert_eq!(trace.borrow().output, b"abc");
    }

    #[test]
    fn downstream_write_failure_still_leaves_the_chunk_in_the_digest() {
        let mut sink = RecordingSink::new(&[1], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        assert!(matches!(
            md5.write(b"abc").unwrap_err(),
            PipelineError::Runtime(_)
        ));

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }

    #[test]
    fn downstream_finish_failure_keeps_the_digest_in_progress() {
        let mut sink = RecordingSink::new(&[], &[1]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        md5.write(b"a").unwrap();
        assert!(matches!(
            md5.finish().unwrap_err(),
            PipelineError::Runtime(_)
        ));
        md5.write(b"b").unwrap();

        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "187ef4436122d1cc2f40dc2b92f0eba0"
        );
    }

    #[test]
    fn no_data_and_an_empty_write_both_report_the_empty_digest() {
        let trace = shared_trace();
        let mut sink = RecordingSink::with_trace(trace.clone(), &[], &[]);
        let mut md5 = PlMd5::new("md5", &mut sink);
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        md5.write(b"").unwrap();
        md5.finish().unwrap();
        assert_eq!(
            md5.get_hex_digest().unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            trace.borrow().calls,
            vec![
                TraceCall::Write {
                    data: Vec::new(),
                    failed: false,
                },
                TraceCall::Finish { failed: false },
            ]
        );
    }
}
